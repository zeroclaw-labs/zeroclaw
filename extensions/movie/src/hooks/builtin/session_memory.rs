use async_trait::async_trait;

use crate::hooks::traits::{HookHandler, HookResult};
use crate::providers::traits::ChatMessage;

/// Built-in hook for lightweight session-memory behavior.
///
/// Current implementation is a safe no-op placeholder that preserves message flow.
pub struct SessionMemoryHook;

#[async_trait]
impl HookHandler for SessionMemoryHook {
    fn name(&self) -> &str {
        "session-memory"
    }

    fn priority(&self) -> i32 {
        -10
    }

    async fn before_compaction(&self, messages: Vec<ChatMessage>) -> HookResult<Vec<ChatMessage>> {
        HookResult::Continue(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_memory_hook_passes_messages_through() {
        let hook = SessionMemoryHook;
        let messages = vec![ChatMessage::user("hello")];
        match hook.before_compaction(messages.clone()).await {
            HookResult::Continue(next) => assert_eq!(next.len(), 1),
            HookResult::Cancel(reason) => panic!("unexpected cancel: {reason}"),
        }
    }
}
