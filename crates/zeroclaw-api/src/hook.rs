use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use crate::channel::ChannelMessage;
use crate::model_provider::{ChatMessage, ChatResponse};
use crate::tool::ToolResult;

/// Opaque runtime context for one tool-call hook phase.
///
/// Correlated contexts carry one stable identity across the before and after
/// phases. Uncorrelated contexts are phase-local and must not be used to pair
/// callbacks or retain attribution-sensitive state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallHookContext {
    invocation_id: String,
    correlated: bool,
}

impl ToolCallHookContext {
    /// Create a correlated context.
    ///
    /// The caller must provide a process-unique identity and reuse it for both
    /// phases of the same tool invocation.
    pub fn new(invocation_id: impl Into<String>) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            correlated: true,
        }
    }

    /// Create a phase-local context when cross-phase identity is unavailable.
    pub fn uncorrelated(invocation_id: impl Into<String>) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            correlated: false,
        }
    }

    /// Return the opaque phase or invocation identity.
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    /// Whether this identity is stable across the before and after phases.
    pub fn is_correlated(&self) -> bool {
        self.correlated
    }
}

/// Result of a modifying hook — continue with (possibly modified) data, or cancel.
#[derive(Debug, Clone)]
pub enum HookResult<T> {
    Continue(T),
    Cancel(String),
}

impl<T> HookResult<T> {
    pub fn is_cancel(&self) -> bool {
        matches!(self, HookResult::Cancel(_))
    }
}

/// Trait for hook handlers. All methods have default no-op implementations.
/// Implement only the events you care about.
#[async_trait]
pub trait HookHandler: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 {
        0
    }

    // --- Void hooks (parallel, fire-and-forget) ---
    async fn on_gateway_start(&self, _host: &str, _port: u16) {}
    async fn on_gateway_stop(&self) {}
    async fn on_session_start(&self, _session_id: &str, _channel: &str) {}
    async fn on_session_end(&self, _session_id: &str, _channel: &str) {}
    async fn on_llm_input(&self, _messages: &[ChatMessage], _model: &str) {}
    async fn on_llm_output(&self, _response: &ChatResponse) {}
    async fn on_after_tool_call(&self, _tool: &str, _result: &ToolResult, _duration: Duration) {}
    /// Observe tool completion with explicit correlation context.
    ///
    /// When `context.is_correlated()` is false, handlers must not use its
    /// phase-local identity to pair callbacks or retain attributed state.
    async fn on_after_tool_call_with_context(
        &self,
        _context: &ToolCallHookContext,
        tool: &str,
        result: &ToolResult,
        duration: Duration,
    ) {
        self.on_after_tool_call(tool, result, duration).await;
    }
    async fn on_message_sent(&self, _channel: &str, _recipient: &str, _content: &str) {}
    async fn on_heartbeat_tick(&self) {}

    // --- Modifying hooks (sequential by priority, can cancel) ---
    async fn before_model_resolve(
        &self,
        model_provider: String,
        model: String,
    ) -> HookResult<(String, String)> {
        HookResult::Continue((model_provider, model))
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        HookResult::Continue(prompt)
    }

    async fn before_llm_call(
        &self,
        _messages: &mut Vec<ChatMessage>,
        _model: &mut String,
    ) -> HookResult<()> {
        HookResult::Continue(())
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        HookResult::Continue((name, args))
    }

    /// Inspect or modify a tool call with explicit correlation context.
    ///
    /// When `context.is_correlated()` is false, handlers must not retain state
    /// that assumes the after phase will receive the same identity.
    async fn before_tool_call_with_context(
        &self,
        _context: &ToolCallHookContext,
        name: String,
        args: Value,
    ) -> HookResult<(String, Value)> {
        self.before_tool_call(name, args).await
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        HookResult::Continue(message)
    }

    async fn on_message_sending(
        &self,
        channel: String,
        recipient: String,
        content: String,
    ) -> HookResult<(String, String, String)> {
        HookResult::Continue((channel, recipient, content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook {
        name: String,
        priority: i32,
    }

    impl TestHook {
        fn new(name: &str, priority: i32) -> Self {
            Self {
                name: name.to_string(),
                priority,
            }
        }
    }

    #[async_trait]
    impl HookHandler for TestHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
    }

    #[test]
    fn hook_result_is_cancel() {
        let ok: HookResult<String> = HookResult::Continue("hi".into());
        assert!(!ok.is_cancel());
        let cancel: HookResult<String> = HookResult::Cancel("blocked".into());
        assert!(cancel.is_cancel());
    }

    #[test]
    fn default_priority_is_zero() {
        struct MinimalHook;
        #[async_trait]
        impl HookHandler for MinimalHook {
            fn name(&self) -> &str {
                "minimal"
            }
        }
        assert_eq!(MinimalHook.priority(), 0);
    }

    #[tokio::test]
    async fn default_modifying_hooks_pass_through() {
        let hook = TestHook::new("test", 0);
        match hook
            .before_tool_call("shell".into(), serde_json::json!({"cmd": "ls"}))
            .await
        {
            HookResult::Continue((name, _args)) => assert_eq!(name, "shell"),
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }

    #[tokio::test]
    async fn context_aware_defaults_delegate_to_legacy_tool_hooks() {
        use std::sync::{Arc, Mutex};

        struct LegacyHook {
            after: Arc<Mutex<Option<(String, bool, Duration)>>>,
        }

        #[async_trait]
        impl HookHandler for LegacyHook {
            fn name(&self) -> &str {
                "legacy"
            }

            async fn before_tool_call(
                &self,
                name: String,
                args: Value,
            ) -> HookResult<(String, Value)> {
                HookResult::Continue((format!("{name}_legacy"), args))
            }

            async fn on_after_tool_call(
                &self,
                tool: &str,
                result: &ToolResult,
                duration: Duration,
            ) {
                *self.after.lock().unwrap() = Some((tool.to_string(), result.success, duration));
            }
        }

        let after = Arc::new(Mutex::new(None));
        let hook = LegacyHook {
            after: Arc::clone(&after),
        };
        let context = ToolCallHookContext::new("opaque-id");
        let args = serde_json::json!({"cmd": "ls"});

        let before = hook
            .before_tool_call_with_context(&context, "shell".into(), args.clone())
            .await;
        match before {
            HookResult::Continue((name, actual_args)) => {
                assert_eq!(name, "shell_legacy");
                assert_eq!(actual_args, args);
            }
            HookResult::Cancel(_) => panic!("legacy hook should continue"),
        }

        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        let duration = Duration::from_millis(12);
        hook.on_after_tool_call_with_context(&context, "shell", &result, duration)
            .await;

        assert_eq!(
            *after.lock().unwrap(),
            Some(("shell".to_string(), true, duration))
        );
    }

    #[test]
    fn tool_call_context_marks_unavailable_correlation() {
        let exact = ToolCallHookContext::new("exact");
        let unavailable = ToolCallHookContext::uncorrelated("legacy");

        assert!(exact.is_correlated());
        assert!(!unavailable.is_correlated());
        assert_eq!(unavailable.invocation_id(), "legacy");
    }
}
