use std::future::Future;

/// Runtime context for the currently processed channel message.
///
/// This context is task-scoped and only available while handling channel
/// messages. CLI/background agent turns do not set this context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRuntimeContext {
    pub channel: String,
    pub reply_target: String,
    pub thread_ts: Option<String>,
    pub sender: String,
    pub message_id: String,
}

tokio::task_local! {
    static CHANNEL_RUNTIME_CONTEXT: ChannelRuntimeContext;
}

/// Run `future` with the provided channel runtime context scoped to this task.
pub async fn with_channel_runtime_context<F, T>(ctx: ChannelRuntimeContext, future: F) -> T
where
    F: Future<Output = T>,
{
    CHANNEL_RUNTIME_CONTEXT.scope(ctx, future).await
}

/// Return the current channel runtime context, if one is set for this task.
pub fn current_channel_runtime_context() -> Option<ChannelRuntimeContext> {
    CHANNEL_RUNTIME_CONTEXT
        .try_with(ChannelRuntimeContext::clone)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context(channel: &str, reply_target: &str) -> ChannelRuntimeContext {
        ChannelRuntimeContext {
            channel: channel.to_string(),
            reply_target: reply_target.to_string(),
            thread_ts: Some("thread-1".to_string()),
            sender: "user_a".to_string(),
            message_id: "msg_123".to_string(),
        }
    }

    #[tokio::test]
    async fn context_unavailable_outside_scope() {
        assert!(current_channel_runtime_context().is_none());
    }

    #[tokio::test]
    async fn context_available_inside_scope() {
        let ctx = sample_context("discord", "C123");
        let seen = with_channel_runtime_context(ctx.clone(), async {
            current_channel_runtime_context().expect("context should be available")
        })
        .await;

        assert_eq!(seen, ctx);
        assert!(current_channel_runtime_context().is_none());
    }

    #[tokio::test]
    async fn parallel_scopes_do_not_leak_context() {
        let ctx_a = sample_context("discord", "C111");
        let ctx_b = sample_context("discord", "C222");

        let (seen_a, seen_b) = tokio::join!(
            with_channel_runtime_context(ctx_a.clone(), async {
                tokio::task::yield_now().await;
                current_channel_runtime_context().expect("context A should be available")
            }),
            with_channel_runtime_context(ctx_b.clone(), async {
                tokio::task::yield_now().await;
                current_channel_runtime_context().expect("context B should be available")
            })
        );

        assert_eq!(seen_a.reply_target, "C111");
        assert_eq!(seen_b.reply_target, "C222");
    }
}
