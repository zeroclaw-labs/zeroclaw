use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use daemonclaw_api::agent::TurnResult;
use daemonclaw_api::channel::ChannelMessage;
use daemonclaw_api::provider::{ChatMessage, ChatResponse};
use daemonclaw_api::tool::ToolResult;

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

/// Control signal returned by `on_turn_complete` hooks.
#[derive(Debug, Clone, Default)]
pub enum TurnCompleteAction {
    /// No opinion — let the loop proceed normally.
    #[default]
    Continue,
    /// Prevent the loop from ending — force another iteration.
    PreventStop,
    /// Inject an error message into history and continue the loop.
    InjectError(String),
    /// Force the loop to stop immediately.
    Stop,
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
    async fn on_message_sent(&self, _channel: &str, _recipient: &str, _content: &str) {}
    async fn on_heartbeat_tick(&self) {}
    async fn on_turn_complete(&self, _result: &TurnResult) -> TurnCompleteAction {
        TurnCompleteAction::Continue
    }

    /// Called between loop iterations (after tool results, before next LLM call).
    /// Returns messages to inject into history (file changes, memory, notifications).
    async fn between_turns(&self, _iteration: usize) -> Vec<ChatMessage> {
        Vec::new()
    }

    /// Called after a turn completes for background fact extraction.
    /// Unlike on_turn_complete, this runs in a spawned task and does not
    /// block the next turn. Use for memory extraction, learning, analytics.
    async fn extract_post_turn(&self, _result: &TurnResult) {}

    /// Generate a follow-up prompt suggestion after a turn completes.
    /// Returns None if no suggestion is appropriate. The suggestion is
    /// surfaced to the UI (MoonWhisp, CLI) but never auto-executed.
    async fn suggest_prompt(&self, _result: &TurnResult) -> Option<String> {
        None
    }

    /// Generate a cheap-model summary of tool usage for the current turn.
    /// Used by UI clients (MoonWhisp) to show progress without full context.
    async fn summarize_tool_use(
        &self,
        _tool_name: &str,
        _tool_args: &serde_json::Value,
        _tool_output: &str,
    ) -> Option<String> {
        None
    }

    /// Called when a delegate sub-agent completes its task.
    async fn on_teammate_task_completed(
        &self,
        _agent_name: &str,
        _result: &str,
    ) {}

    /// Called when a delegate sub-agent becomes idle (no pending work).
    async fn on_teammate_idle(&self, _agent_name: &str) {}

    // --- Modifying hooks (sequential by priority, can cancel) ---
    async fn before_model_resolve(
        &self,
        provider: String,
        model: String,
    ) -> HookResult<(String, String)> {
        HookResult::Continue((provider, model))
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        HookResult::Continue(prompt)
    }

    async fn before_llm_call(
        &self,
        messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        HookResult::Continue((messages, model))
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        HookResult::Continue((name, args))
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
}
