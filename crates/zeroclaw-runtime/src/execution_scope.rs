//! Bounded caller context for the existing turn engine. Domain controllers own
//! their records; this adapter exposes attribution and admission callbacks only.
use std::future::Future;
use std::sync::Arc;

pub trait ExecutionScopeObserver: Send + Sync {
    fn goal_id(&self) -> Option<String> {
        None
    }
    fn context_isolated(&self) -> bool {
        false
    }
    fn check_budget(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn record_usage_error(&self, _error: &anyhow::Error) {}
    fn record_approval_required(&self, _tool: &str, _args: &serde_json::Value) {}
    /// Controllers may accept a trusted actor's one-way publication. The
    /// default never starts a recipient or exposes any recipient result.
    fn publish_peer_message(
        &self,
        _sender: &str,
        _recipient: &str,
        _content: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

tokio::task_local! { static EXECUTION_SCOPE: Arc<dyn ExecutionScopeObserver>; }

pub async fn scope<F: Future>(observer: Arc<dyn ExecutionScopeObserver>, future: F) -> F::Output {
    EXECUTION_SCOPE.scope(observer, future).await
}

pub fn current_goal_id() -> Option<String> {
    EXECUTION_SCOPE
        .try_with(|scope| scope.goal_id())
        .ok()
        .flatten()
}

pub fn context_isolated() -> bool {
    EXECUTION_SCOPE
        .try_with(|scope| scope.context_isolated())
        .unwrap_or(false)
}

pub fn publish_peer_message(
    sender: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<Option<String>> {
    EXECUTION_SCOPE
        .try_with(|scope| scope.publish_peer_message(sender, recipient, content))
        .unwrap_or(Ok(None))
}

pub(crate) fn check_budget() -> anyhow::Result<()> {
    EXECUTION_SCOPE
        .try_with(|scope| scope.check_budget())
        .unwrap_or(Ok(()))
}

pub(crate) fn record_usage_error(error: &anyhow::Error) {
    let _ = EXECUTION_SCOPE.try_with(|scope| scope.record_usage_error(error));
}

pub(crate) fn record_approval_required(tool: &str, args: &serde_json::Value) {
    let _ = EXECUTION_SCOPE.try_with(|scope| scope.record_approval_required(tool, args));
}
