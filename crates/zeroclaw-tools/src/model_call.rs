//! Admission boundary for model calls made by tools.

use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use zeroclaw_api::tool::ToolExecutionContext;

/// An unpolled future containing the complete provider dispatch.
pub type ToolModelCallFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;

/// Result of a tool-owned model call and its admission or settlement.
#[must_use]
pub enum ToolModelCallOutcome {
    /// The call completed with its ordinary result.
    Completed(anyhow::Result<String>),
    /// Admission was refused before the call future was polled.
    Refused(anyhow::Error),
    /// A Goal stop occurred; finish only local cleanup and return the stop as an error.
    Stopped {
        call: ToolModelCallDisposition,
        stop: anyhow::Error,
    },
}

/// The provider call's disposition when subsequent Goal settlement stops work.
#[derive(Debug)]
pub enum ToolModelCallDisposition {
    Succeeded,
    Failed { non_retryable: bool },
}

/// Admits a tool model call before polling its complete dispatch future.
#[async_trait]
pub trait ToolModelCallGate: Send + Sync {
    async fn run(
        &self,
        provider_ref: &str,
        model: &str,
        call: ToolModelCallFuture<'_>,
    ) -> ToolModelCallOutcome;
}

/// Direct dispatch outside Goals; refuses Goal work without an accounting host.
pub struct DirectToolModelCall;

#[async_trait]
impl ToolModelCallGate for DirectToolModelCall {
    async fn run(
        &self,
        provider_ref: &str,
        model: &str,
        call: ToolModelCallFuture<'_>,
    ) -> ToolModelCallOutcome {
        if ToolExecutionContext::current().is_some_and(|context| context.is_goal_foreground()) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": "tool.model_call.goal_unsupported",
                        "provider_ref": provider_ref,
                        "model": model,
                    })),
                "tool model call refused inside a Goal: no Goal accounting gate"
            );
            return ToolModelCallOutcome::Refused(anyhow::Error::msg(
                crate::i18n::get_required_tool_string("tool-model-call-gate-goal-unsupported"),
            ));
        }
        ToolModelCallOutcome::Completed(call.await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn direct_gate_refuses_goal_before_polling_but_preserves_ordinary_calls() {
        let polls = AtomicUsize::new(0);
        let call = || {
            Box::pin(async {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok("ordinary result".to_owned())
            }) as ToolModelCallFuture<'_>
        };
        let context = ToolExecutionContext::default().with_goal_foreground();
        let refused = context
            .scope_only(DirectToolModelCall.run("fixture.primary", "model", call()))
            .await;
        match refused {
            ToolModelCallOutcome::Refused(error) => {
                assert_eq!(
                    error.to_string(),
                    crate::i18n::get_required_tool_string("tool-model-call-gate-goal-unsupported")
                );
                assert!(!error.is::<zeroclaw_api::tool::GoalChildOperationError>());
            }
            _ => panic!("Goal call was not refused"),
        }
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        match DirectToolModelCall
            .run("fixture.primary", "model", call())
            .await
        {
            ToolModelCallOutcome::Completed(Ok(result)) => assert_eq!(result, "ordinary result"),
            _ => panic!("ordinary call did not complete"),
        }
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }
}
