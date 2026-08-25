//! Private, poll-owned provider-attempt collection.
//!
//! A dispatch node exists only after its future or stream has actually been
//! polled.  Composite providers mark their current node before dispatching a
//! child; the immutable close-time projection then contains physical leaves
//! only.  This deliberately carries no request payload or error text.

use super::{AccountedAttempt, AttemptUsageOutcome, InvalidUsageReason};
use crate::traits::{ChatResponse, TokenUsage};
use futures_util::StreamExt as _;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::sync::Arc;

tokio::task_local! {
    static ACTIVE_COLLECTOR: Arc<Mutex<CollectorState>>;
    static POLL_STACK: RefCell<Vec<(usize, DispatchNodeId)>>;
    static ROUTE_STACK: RefCell<Vec<(usize, Arc<RouteIdentity>)>>;
}

#[derive(Debug)]
struct RouteIdentity {
    provider_ref: String,
    model: String,
}

fn collector_key(collector: &Arc<Mutex<CollectorState>>) -> usize {
    Arc::as_ptr(collector) as usize
}

struct PollFrame(usize, DispatchNodeId);
impl Drop for PollFrame {
    fn drop(&mut self) {
        let _ = POLL_STACK.try_with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some((self.0, self.1)));
        });
    }
}

struct RouteFrame(bool);
impl Drop for RouteFrame {
    fn drop(&mut self) {
        if self.0 {
            let _ = ROUTE_STACK.try_with(|routes| {
                routes.borrow_mut().pop();
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DispatchNodeId(usize);

#[derive(Debug)]
struct Node {
    parent: Option<DispatchNodeId>,
    provider_ref: String,
    model: String,
    composite: bool,
    finalized: bool,
    outcome: Option<AttemptUsageOutcome>,
}

#[derive(Debug, Default)]
struct CollectorState {
    closed: bool,
    logical_success: bool,
    nodes: Vec<Node>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CallAccountingCollector {
    state: Arc<Mutex<CollectorState>>,
}

impl CallAccountingCollector {
    pub(crate) async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        ACTIVE_COLLECTOR
            .scope(
                Arc::clone(&self.state),
                POLL_STACK.scope(
                    RefCell::new(Vec::new()),
                    ROUTE_STACK.scope(RefCell::new(Vec::new()), future),
                ),
            )
            .await
    }

    pub(crate) fn mark_logical_success(&self) {
        let mut state = self.state.lock();
        if !state.closed {
            state.logical_success = true;
        }
    }

    pub(crate) fn close(&self) -> (Vec<AccountedAttempt>, Option<(String, String)>) {
        let (nodes, logical_success) = {
            let mut state = self.state.lock();
            if state.closed {
                return (Vec::new(), None);
            }
            state.closed = true;
            for node in &mut state.nodes {
                if !node.finalized {
                    node.outcome = Some(AttemptUsageOutcome::OutcomeUnknown {
                        observed: node
                            .outcome
                            .as_ref()
                            .and_then(AttemptUsageOutcome::observed_usage),
                    });
                }
            }
            (std::mem::take(&mut state.nodes), state.logical_success)
        };
        let successful_route = if logical_success {
            nodes
                .iter()
                .rev()
                .find(|node| node.finalized && !node.composite)
                .map(|node| (node.provider_ref.clone(), node.model.clone()))
        } else {
            None
        };
        let attempts = nodes
            .into_iter()
            .filter(|node| !node.composite)
            .map(|node| {
                AccountedAttempt::new(
                    node.provider_ref,
                    node.model,
                    node.outcome
                        .unwrap_or(AttemptUsageOutcome::OutcomeUnknown { observed: None }),
                )
            })
            .collect();
        (attempts, successful_route)
    }
}

/// A first-poll lease.  It owns no lock while an inner provider is polled.
pub(crate) struct AttemptLease {
    collector: Arc<Mutex<CollectorState>>,
    node: DispatchNodeId,
}

pub(crate) enum AttemptState {
    Unstarted { provider_ref: String, model: String },
    Active(AttemptLease),
    Disabled,
}

impl AttemptState {
    pub(crate) fn unstarted(provider_ref: String, model: String) -> Self {
        Self::Unstarted {
            provider_ref,
            model,
        }
    }

    pub(crate) fn start(&mut self) {
        if !matches!(self, Self::Unstarted { .. }) {
            return;
        }
        let Self::Unstarted {
            provider_ref,
            model,
        } = std::mem::replace(self, Self::Disabled)
        else {
            unreachable!("the unstarted guard above must hold");
        };
        *self = match AttemptLease::begin(provider_ref, model) {
            Some(lease) => Self::Active(lease),
            None => Self::Disabled,
        };
    }

    pub(crate) fn lease(&self) -> Option<&AttemptLease> {
        match self {
            Self::Active(lease) => Some(lease),
            Self::Unstarted { .. } | Self::Disabled => None,
        }
    }
}

impl AttemptLease {
    pub(crate) fn begin(provider_ref: String, model: String) -> Option<Self> {
        ACTIVE_COLLECTOR
            .try_with(|collector| {
                let mut state = collector.lock();
                if state.closed {
                    return None;
                }
                let key = collector_key(collector);
                let synchronous_parent = POLL_STACK
                    .try_with(|stack| {
                        stack
                            .borrow()
                            .iter()
                            .rev()
                            .find_map(|(stack_key, node)| (*stack_key == key).then_some(*node))
                    })
                    .ok()
                    .flatten();
                let parent = synchronous_parent;
                let node = DispatchNodeId(state.nodes.len());
                let route = ROUTE_STACK
                    .try_with(|routes| {
                        routes.borrow().iter().rev().find_map(|(route_key, route)| {
                            (*route_key == key)
                                .then(|| (route.provider_ref.clone(), route.model.clone()))
                        })
                    })
                    .ok()
                    .flatten();
                let (provider_ref, model) = route.unwrap_or((provider_ref, model));
                state.nodes.push(Node {
                    parent,
                    provider_ref,
                    model,
                    composite: false,
                    finalized: false,
                    outcome: None,
                });
                if let Some(parent) = parent {
                    state.nodes[parent.0].composite = true;
                }
                Some(Self {
                    collector: Arc::clone(collector),
                    node,
                })
            })
            .ok()
            .flatten()
    }

    pub(crate) fn poll_scope<R>(&self, poll: impl FnOnce() -> R) -> R {
        let key = collector_key(&self.collector);
        let _ = POLL_STACK.try_with(|stack| stack.borrow_mut().push((key, self.node)));
        let _frame = PollFrame(key, self.node);
        poll()
    }

    pub(crate) fn finish_response(&self, response: &ChatResponse) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state.nodes.get_mut(self.node.0) {
            node.outcome = Some(classify_usage(response.usage.clone()));
            node.finalized = true;
        }
    }

    pub(crate) fn finish_missing_response(&self) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state.nodes.get_mut(self.node.0) {
            node.outcome = Some(AttemptUsageOutcome::Missing);
            node.finalized = true;
        }
    }

    pub(crate) fn finish_error_with_usage(&self, usage: Option<TokenUsage>) {
        let Some(usage) = usage else {
            self.set_unknown();
            return;
        };
        match classify_usage(Some(usage)) {
            AttemptUsageOutcome::Complete(usage) => self.set_unknown_with_observed(Some(usage)),
            AttemptUsageOutcome::Invalid { .. }
            | AttemptUsageOutcome::Missing
            | AttemptUsageOutcome::OutcomeUnknown { .. } => self.set_unknown_with_observed(None),
        }
    }

    pub(crate) fn observe_stream_usage(&self, usage: TokenUsage) {
        self.set_outcome(classify_usage(Some(usage)));
    }

    pub(crate) fn finish_stream(&self) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        let Some(node) = state.nodes.get_mut(self.node.0) else {
            return;
        };
        if node.outcome.is_none() {
            node.outcome = Some(AttemptUsageOutcome::Missing);
        }
        node.finalized = true;
        if node.parent.is_none() {
            state.logical_success = true;
        }
    }

    pub(crate) fn set_unknown(&self) {
        self.set_unknown_with_observed(None);
    }

    fn set_unknown_with_observed(&self, observed: Option<TokenUsage>) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        let Some(node) = state.nodes.get_mut(self.node.0) else {
            return;
        };
        if !node.finalized {
            node.outcome = Some(AttemptUsageOutcome::OutcomeUnknown {
                observed: observed.or_else(|| {
                    node.outcome
                        .as_ref()
                        .and_then(AttemptUsageOutcome::observed_usage)
                }),
            });
        }
    }

    fn set_outcome(&self, outcome: AttemptUsageOutcome) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state.nodes.get_mut(self.node.0)
            && !node.finalized
        {
            node.outcome = Some(outcome);
        }
    }
}

pub(crate) fn exact_route_future<F>(
    provider_ref: String,
    model: String,
    future: F,
) -> impl std::future::Future<Output = F::Output>
where
    F: std::future::Future,
{
    let mut future = Box::pin(future);
    let route = Arc::new(RouteIdentity {
        provider_ref,
        model,
    });
    futures_util::future::poll_fn(move |cx| {
        let key = ACTIVE_COLLECTOR.try_with(collector_key).ok();
        let pushed = if let Some(key) = key {
            ROUTE_STACK
                .try_with(|routes| routes.borrow_mut().push((key, Arc::clone(&route))))
                .is_ok()
        } else {
            false
        };
        let _frame = RouteFrame(pushed);
        future.as_mut().poll(cx)
    })
}

pub(crate) fn exact_route_stream<T>(
    provider_ref: String,
    model: String,
    stream: futures_util::stream::BoxStream<'static, T>,
) -> futures_util::stream::BoxStream<'static, T>
where
    T: Send + 'static,
{
    let mut stream = stream;
    let route = Arc::new(RouteIdentity {
        provider_ref,
        model,
    });
    futures_util::stream::poll_fn(move |cx| {
        let key = ACTIVE_COLLECTOR.try_with(collector_key).ok();
        let pushed = if let Some(key) = key {
            ROUTE_STACK
                .try_with(|routes| routes.borrow_mut().push((key, Arc::clone(&route))))
                .is_ok()
        } else {
            false
        };
        let _frame = RouteFrame(pushed);
        stream.as_mut().poll_next(cx)
    })
    .boxed()
}

impl Drop for AttemptLease {
    fn drop(&mut self) {
        self.set_unknown();
    }
}

/// Mark the currently polled dispatch provider as a composite.  Calls outside
/// a scoped dispatch are intentionally harmless for ordinary trait callers.
pub(crate) fn mark_current_composite() {
    let _ = ACTIVE_COLLECTOR.try_with(|collector| {
        let key = collector_key(collector);
        let node = POLL_STACK
            .try_with(|stack| {
                stack
                    .borrow()
                    .iter()
                    .rev()
                    .find_map(|(stack_key, node)| (*stack_key == key).then_some(*node))
            })
            .ok()
            .flatten();
        let Some(node) = node else {
            return;
        };
        let mut state = collector.lock();
        if !state.closed
            && let Some(current) = state.nodes.get_mut(node.0)
        {
            current.composite = true;
        }
    });
}

/// Attach a runtime-observed final stream usage snapshot to the latest open
/// physical leaf. This does not manufacture a node; it only preserves the
/// lower bound already observed by the stream consumer.
pub(crate) fn record_stream_interruption_usage(usage: TokenUsage) {
    let _ = ACTIVE_COLLECTOR.try_with(|collector| {
        let mut state = collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state
            .nodes
            .iter_mut()
            .rev()
            .find(|node| !node.composite && !node.finalized)
        {
            node.outcome = Some(match classify_usage(Some(usage)) {
                AttemptUsageOutcome::Complete(usage) => AttemptUsageOutcome::OutcomeUnknown {
                    observed: Some(usage),
                },
                AttemptUsageOutcome::Invalid { .. } => {
                    AttemptUsageOutcome::OutcomeUnknown { observed: None }
                }
                AttemptUsageOutcome::Missing | AttemptUsageOutcome::OutcomeUnknown { .. } => {
                    AttemptUsageOutcome::OutcomeUnknown { observed: None }
                }
            });
        }
    });
}

/// Reclassify the latest finalized stream leaf when runtime rejects an
/// otherwise terminal transport result semantically. This is not a late stream
/// event: the response can no longer be accepted, but its valid usage remains
/// an interrupted lower bound.
pub(crate) fn record_stream_semantic_rejection_usage(usage: TokenUsage) {
    let _ = ACTIVE_COLLECTOR.try_with(|collector| {
        let mut state = collector.lock();
        if state.closed {
            return;
        }
        state.logical_success = false;
        if let Some(node) = state.nodes.iter_mut().rev().find(|node| !node.composite) {
            node.outcome = Some(match classify_usage(Some(usage)) {
                AttemptUsageOutcome::Complete(usage) => AttemptUsageOutcome::OutcomeUnknown {
                    observed: Some(usage),
                },
                invalid @ AttemptUsageOutcome::Invalid { .. } => invalid,
                AttemptUsageOutcome::Missing | AttemptUsageOutcome::OutcomeUnknown { .. } => {
                    AttemptUsageOutcome::OutcomeUnknown { observed: None }
                }
            });
        }
    });
}

/// Compatibility-only live projection used to retain typed terminal-error
/// usage while the provider future is still unwinding.  The collector nodes,
/// not Reliable, remain the sole attempt store.
pub(crate) fn current_billable_usage() -> Option<TokenUsage> {
    ACTIVE_COLLECTOR
        .try_with(|collector| {
            let state = collector.lock();
            let mut total: Option<TokenUsage> = None;
            for node in &state.nodes {
                if node.composite {
                    continue;
                }
                let usage = match node.outcome.as_ref() {
                    Some(AttemptUsageOutcome::Complete(usage)) => Some(usage),
                    Some(AttemptUsageOutcome::OutcomeUnknown {
                        observed: Some(usage),
                    }) => Some(usage),
                    _ => None,
                };
                if let Some(usage) = usage {
                    let input = usage.input_tokens?;
                    let output = usage.output_tokens?;
                    let cached = usage.cached_input_tokens.unwrap_or(0);
                    let next = TokenUsage {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        cached_input_tokens: Some(cached),
                    };
                    match &mut total {
                        Some(total) => {
                            total.input_tokens = total.input_tokens?.checked_add(input);
                            total.output_tokens = total.output_tokens?.checked_add(output);
                            total.cached_input_tokens =
                                total.cached_input_tokens?.checked_add(cached);
                        }
                        None => total = Some(next),
                    }
                }
            }
            total
        })
        .ok()
        .flatten()
}

fn classify_usage(usage: Option<TokenUsage>) -> AttemptUsageOutcome {
    let Some(usage) = usage else {
        return AttemptUsageOutcome::Missing;
    };
    if usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cached_input_tokens.is_none()
    {
        return AttemptUsageOutcome::Missing;
    }
    let Some(input) = usage.input_tokens else {
        return AttemptUsageOutcome::Invalid {
            observed: usage,
            reason: InvalidUsageReason::MissingInput,
        };
    };
    let Some(output) = usage.output_tokens else {
        return AttemptUsageOutcome::Invalid {
            observed: usage,
            reason: InvalidUsageReason::MissingOutput,
        };
    };
    if usage
        .cached_input_tokens
        .is_some_and(|cached| cached > input)
    {
        return AttemptUsageOutcome::Invalid {
            observed: usage,
            reason: InvalidUsageReason::CachedInputExceedsInput,
        };
    }
    if input.checked_add(output).is_none() {
        return AttemptUsageOutcome::Invalid {
            observed: usage,
            reason: InvalidUsageReason::TotalOverflow,
        };
    }
    AttemptUsageOutcome::Complete(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_classification_preserves_missing_zero_and_invalid_distinctions() {
        assert!(matches!(classify_usage(None), AttemptUsageOutcome::Missing));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
            })),
            AttemptUsageOutcome::Missing
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                cached_input_tokens: Some(0),
            })),
            AttemptUsageOutcome::Complete(_)
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: None,
                output_tokens: Some(1),
                cached_input_tokens: None,
            })),
            AttemptUsageOutcome::Invalid {
                reason: InvalidUsageReason::MissingInput,
                ..
            }
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: Some(1),
                output_tokens: Some(1),
                cached_input_tokens: Some(2),
            })),
            AttemptUsageOutcome::Invalid {
                reason: InvalidUsageReason::CachedInputExceedsInput,
                ..
            }
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: Some(u64::MAX),
                output_tokens: Some(1),
                cached_input_tokens: None,
            })),
            AttemptUsageOutcome::Invalid {
                reason: InvalidUsageReason::TotalOverflow,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn close_finalizes_pending_once_and_rejects_late_writes() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;

        let (report, successful_route) = collector.close();
        assert_eq!(report.len(), 1);
        assert!(successful_route.is_none());
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            cached_input_tokens: None,
        });
        assert!(collector.close().0.is_empty());
    }

    #[tokio::test]
    async fn close_turns_unfinished_usage_snapshot_into_unknown_lower_bound() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            cached_input_tokens: Some(1),
        });

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    cached_input_tokens: Some(1),
                })
            }
        ));
    }

    #[tokio::test]
    async fn close_turns_unfinished_invalid_usage_into_unknown_without_a_lower_bound() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;
        lease.observe_stream_usage(TokenUsage {
            input_tokens: None,
            output_tokens: Some(2),
            cached_input_tokens: None,
        });

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
    }

    #[tokio::test]
    async fn runtime_interruption_cannot_restore_invalid_usage_after_an_unresolved_stream_end() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                let lease =
                    AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                        .expect("scoped first poll creates a lease");
                let invalid = TokenUsage {
                    input_tokens: None,
                    output_tokens: Some(2),
                    cached_input_tokens: None,
                };
                lease.observe_stream_usage(invalid.clone());
                lease.set_unknown();
                record_stream_interruption_usage(invalid);
            })
            .await;

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
    }

    #[tokio::test]
    async fn poll_scope_restores_the_stack_after_a_completed_poll() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                let lease =
                    AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                        .expect("scoped first poll creates a lease");
                lease.poll_scope(|| ());
                let key = collector_key(&lease.collector);
                assert!(
                    POLL_STACK
                        .try_with(|stack| {
                            stack
                                .borrow()
                                .iter()
                                .all(|(stack_key, _)| *stack_key != key)
                        })
                        .expect("collector scope installs a poll stack")
                );
            })
            .await;
    }

    #[tokio::test]
    async fn final_stream_outcome_ignores_later_usage_snapshots() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;
        lease.finish_stream();
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            cached_input_tokens: None,
        });

        let (report, _) = collector.close();
        assert!(matches!(report[0].outcome(), AttemptUsageOutcome::Missing));
    }

    #[tokio::test]
    async fn unfinished_stream_replaces_cumulative_usage_snapshot_before_close() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;

        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            cached_input_tokens: Some(1),
        });
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(15),
            output_tokens: Some(3),
            cached_input_tokens: Some(2),
        });

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage {
                    input_tokens: Some(15),
                    output_tokens: Some(3),
                    cached_input_tokens: Some(2),
                })
            }
        ));
    }

    #[tokio::test]
    async fn close_wins_against_a_late_stream_write() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;

        let (report, _) = collector.close();
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(99),
            output_tokens: Some(1),
            cached_input_tokens: None,
        });

        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
        assert!(
            collector.close().0.is_empty(),
            "late writes cannot reopen a report"
        );
    }

    #[tokio::test]
    async fn aborted_task_dropping_a_started_lease_reports_unknown() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;
        let task = zeroclaw_spawn::spawn!(async move {
            let _lease = lease;
            futures_util::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
        ));
    }

    #[tokio::test]
    async fn close_and_late_write_race_is_atomic_and_never_reopens() {
        let collector = CallAccountingCollector::default();
        let lease = collector
            .scope(async {
                AttemptLease::begin("configured.provider".to_string(), "model".to_string())
                    .expect("scoped first poll creates a lease")
            })
            .await;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writer_barrier = Arc::clone(&barrier);
        let writer = std::thread::spawn(move || {
            writer_barrier.wait();
            lease.observe_stream_usage(TokenUsage {
                input_tokens: Some(9),
                output_tokens: Some(1),
                cached_input_tokens: None,
            });
        });

        barrier.wait();
        let (report, _) = collector.close();
        writer.join().expect("writer thread must finish");

        assert_eq!(report.len(), 1);
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: None }
                | AttemptUsageOutcome::OutcomeUnknown { observed: Some(_) }
        ));
        assert!(
            collector.close().0.is_empty(),
            "closed collector cannot reopen"
        );
    }
}
