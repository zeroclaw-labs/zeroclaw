//! Admission and settlement for model calls made by Goal-owned tools and SOPs.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::time::Instant;
use zeroclaw_api::{
    tool::{GoalChildOperationError, ToolExecutionCancelled},
    turn_stop::{TurnStop, TurnStopCode},
};
use zeroclaw_goal::accounting::{
    GOAL_OPERATION_ACCOUNTING, GoalOperationAccounting, GoalOrdinarySpendRefusal,
};
use zeroclaw_providers::dispatch::with_exact_dispatch_route;
use zeroclaw_tools::model_call::{
    ToolModelCallDisposition, ToolModelCallFuture, ToolModelCallGate, ToolModelCallOutcome,
};

use crate::agent::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext, TurnUsage,
    admit_goal_operation_if_scoped, begin_goal_dispatch_if_scoped, new_provider_attempt_scope,
    register_goal_returned_attempts, release_after_first_poll,
    release_unstarted_goal_operation_if_scoped, settle_interrupted_provider_attempts,
    settle_provider_attempts, with_settlement_failure,
};
use crate::agent::turn::provider_call::enforce_tool_loop_budget;
use crate::goal_accounting::GoalForegroundContext;

/// Values captured on the Goal task for an accounted call on another runtime.
pub(crate) struct GoalModelCallTicket {
    context: GoalForegroundContext,
    accounting: Arc<dyn GoalOperationAccounting>,
    cost: Option<ToolLoopCostTrackingContext>,
    turn_usage: Option<Arc<Mutex<TurnUsage>>>,
}

pub(crate) enum GoalModelCallPrecheck {
    Ordinary,
    Refused(anyhow::Error),
    Admissible(Box<GoalModelCallTicket>),
}

fn marked(error: anyhow::Error) -> anyhow::Error {
    error.context(GoalChildOperationError)
}

fn incomplete_goal_scope() -> anyhow::Error {
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
        "Goal model call refused: incomplete foreground scope"
    );
    marked(anyhow::Error::msg("incomplete Goal foreground scope"))
}

/// Capture the existing scopes before moving a future to a bridge runtime.
pub(crate) fn goal_model_call_precheck() -> GoalModelCallPrecheck {
    if !GoalForegroundContext::is_participating() {
        return GoalModelCallPrecheck::Ordinary;
    }
    let context = match GoalForegroundContext::current_for_child() {
        Ok(Some(context)) => context,
        Ok(None) => {
            return GoalModelCallPrecheck::Refused(incomplete_goal_scope());
        }
        Err(error) => return GoalModelCallPrecheck::Refused(marked(error)),
    };
    let Some(accounting) = GOAL_OPERATION_ACCOUNTING
        .try_with(Clone::clone)
        .ok()
        .flatten()
    else {
        return GoalModelCallPrecheck::Refused(incomplete_goal_scope());
    };
    if let Err(error) = enforce_tool_loop_budget() {
        return GoalModelCallPrecheck::Refused(marked(
            GoalOrdinarySpendRefusal::new(error, true).into(),
        ));
    }
    if context.token().is_cancelled()
        || context
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return GoalModelCallPrecheck::Refused(marked(ToolExecutionCancelled.into()));
    }
    GoalModelCallPrecheck::Admissible(Box::new(GoalModelCallTicket {
        context,
        accounting,
        cost: TOOL_LOOP_COST_TRACKING_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten(),
        turn_usage: TOOL_LOOP_TURN_USAGE.try_with(Clone::clone).ok().flatten(),
    }))
}

/// Run an admitted Goal-owned call and settle its physical attempts before returning.
pub(crate) async fn run_goal_model_call(
    ticket: GoalModelCallTicket,
    provider_ref: &str,
    model: &str,
    local_timeout: Option<Duration>,
    call: ToolModelCallFuture<'_>,
) -> ToolModelCallOutcome {
    let context = ticket
        .context
        .with_deadline(local_timeout.map(|bound| Instant::now() + bound));
    let operation = async {
        let _admission = match admit_goal_operation_if_scoped(provider_ref, model).await {
            Ok(admission) => admission,
            Err(error) => return ToolModelCallOutcome::Refused(marked(error)),
        };
        let refuse_unstarted = |error| async {
            let error = match release_unstarted_goal_operation_if_scoped().await {
                Ok(()) => error,
                Err(secondary) => with_settlement_failure(error, secondary),
            };
            ToolModelCallOutcome::Refused(marked(error))
        };
        let start = match begin_goal_dispatch_if_scoped().await {
            Ok(start) => start,
            Err(error) => return refuse_unstarted(error).await,
        };
        let accounting = new_provider_attempt_scope();
        let mut dispatch_polled = false;
        let outcome = {
            let request = release_after_first_poll(start, async {
                dispatch_polled = true;
                accounting
                    .scope(with_exact_dispatch_route(
                        provider_ref.to_owned(),
                        model.to_owned(),
                        call,
                    ))
                    .await
            });
            if let Some(deadline) = context.deadline() {
                tokio::select! {
                    biased;
                    () = context.token().cancelled() => Err(anyhow::Error::new(ToolExecutionCancelled)),
                    () = tokio::time::sleep_until(deadline) => {
                        context.token().cancel();
                        Err(anyhow::Error::new(ToolExecutionCancelled).context(TurnStop::recoverable(
                            TurnStopCode::TurnTimeout, "Goal model call deadline elapsed")))
                    },
                    result = request => Ok(result),
                }
            } else {
                tokio::select! {
                    biased;
                    () = context.token().cancelled() => Err(anyhow::Error::new(ToolExecutionCancelled)),
                    result = request => Ok(result),
                }
            }
        };
        let result = match outcome {
            Ok(result) => result,
            Err(error) if !dispatch_polled => return refuse_unstarted(error).await,
            Err(error) => {
                // Dropping the request exposes its observed attempts before settling it.
                let report = accounting.take();
                let error = match settle_interrupted_provider_attempts(report.attempts()).await {
                    Ok(()) => error,
                    Err(secondary) => with_settlement_failure(error, secondary),
                };
                return ToolModelCallOutcome::Stopped {
                    call: ToolModelCallDisposition::Failed {
                        non_retryable: zeroclaw_providers::reliable::is_non_retryable(&error),
                    },
                    stop: marked(error),
                };
            }
        };
        if result.is_ok() {
            accounting.mark_logical_success();
        }
        let report = accounting.take();
        let attempts = report.attempts();
        register_goal_returned_attempts(attempts);
        let accepted = result.is_ok().then_some(attempts.len().saturating_sub(1));
        let settlement = settle_provider_attempts(attempts, accepted).await;
        if result.is_ok() && attempts.is_empty() {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "Goal tool model call produced no accounted provider attempt"
            );
            let error =
                anyhow::Error::msg("Goal tool model call produced no accounted provider attempt");
            let error = match settlement {
                Ok(()) => error,
                Err(secondary) => with_settlement_failure(error, secondary),
            };
            return ToolModelCallOutcome::Stopped {
                call: ToolModelCallDisposition::Succeeded,
                stop: marked(error),
            };
        }
        match (result, settlement) {
            (Ok(value), Ok(())) => ToolModelCallOutcome::Completed(Ok(value)),
            (Ok(_), Err(error)) => ToolModelCallOutcome::Stopped {
                call: ToolModelCallDisposition::Succeeded,
                stop: marked(error),
            },
            (Err(error), settlement) => {
                let non_retryable = zeroclaw_providers::reliable::is_non_retryable(&error);
                let error = match settlement {
                    Ok(()) => error,
                    Err(secondary) => with_settlement_failure(error, secondary),
                };
                ToolModelCallOutcome::Stopped {
                    call: ToolModelCallDisposition::Failed { non_retryable },
                    stop: marked(error),
                }
            }
        }
    };
    GOAL_OPERATION_ACCOUNTING
        .scope(
            Some(ticket.accounting),
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                ticket.cost,
                TOOL_LOOP_TURN_USAGE.scope(ticket.turn_usage, context.scope(operation)),
            ),
        )
        .await
}

pub(crate) struct RuntimeToolModelCallGate;

#[async_trait]
impl ToolModelCallGate for RuntimeToolModelCallGate {
    async fn run(
        &self,
        provider_ref: &str,
        model: &str,
        call: ToolModelCallFuture<'_>,
    ) -> ToolModelCallOutcome {
        match goal_model_call_precheck() {
            GoalModelCallPrecheck::Ordinary => ToolModelCallOutcome::Completed(call.await),
            GoalModelCallPrecheck::Refused(error) => ToolModelCallOutcome::Refused(error),
            GoalModelCallPrecheck::Admissible(ticket) => {
                run_goal_model_call(*ticket, provider_ref, model, None, call).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::tool::ToolExecutionContext;

    #[tokio::test]
    async fn partial_goal_scope_is_refused_before_provider_future_is_polled() {
        let polls = AtomicUsize::new(0);
        let context = ToolExecutionContext::default().with_goal_foreground();
        let result = context
            .scope_only(RuntimeToolModelCallGate.run(
                "test.route",
                "test-model",
                Box::pin(async {
                    polls.fetch_add(1, Ordering::SeqCst);
                    Ok("unexpected".to_owned())
                }),
            ))
            .await;
        match result {
            ToolModelCallOutcome::Refused(error) => assert!(error.is::<GoalChildOperationError>()),
            _ => panic!("partial Goal scope was not refused"),
        }
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ordinary_call_is_forwarded_without_goal_accounting() {
        let result = RuntimeToolModelCallGate
            .run(
                "test.route",
                "test-model",
                Box::pin(async { Ok("ordinary".to_owned()) }),
            )
            .await;
        match result {
            ToolModelCallOutcome::Completed(Ok(value)) => assert_eq!(value, "ordinary"),
            _ => panic!("ordinary call did not complete"),
        }
    }
}
