//! Private, poll-owned provider-attempt collection.
//!
//! A dispatch node exists only after its future or stream has actually been
//! polled.  Composite providers mark their current node before dispatching a
//! child; the immutable close-time projection then contains physical leaves
//! only, expanded when a provider reports multiple model attempts in one
//! response. This deliberately carries no request payload or error text.

use super::{AccountedAttempt, AttemptUsageOutcome, InvalidUsageReason, UNATTRIBUTED_MODEL};
use crate::traits::{ChatResponse, TokenUsage};
use futures_util::StreamExt as _;
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
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

/// One earlier attempt reported inside a completed provider response.
#[derive(Debug, Clone)]
pub(crate) struct IterationRecord {
    pub(crate) model: Option<String>,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) well_formed: bool,
}

/// The returned attempt and whether its two usage observations agree.
#[derive(Debug, Clone)]
pub(crate) struct TerminalRecord {
    pub(crate) model: Option<String>,
    pub(crate) settled: bool,
    pub(crate) malformed: bool,
    pub(crate) observation: Option<TokenUsage>,
}

/// Payload-free usage observations attached by the wire owner after decoding.
#[derive(Debug, Clone)]
pub(crate) struct UsageChain {
    pub(crate) prior: Vec<IterationRecord>,
    pub(crate) terminal: TerminalRecord,
}

#[derive(Debug)]
struct Node {
    parent: Option<DispatchNodeId>,
    provider_ref: String,
    model: String,
    composite: bool,
    suppressed: bool,
    finalized: bool,
    string_usage_eligible: bool,
    outcome: Option<AttemptUsageOutcome>,
    chain: Option<UsageChain>,
}

/// Project one transport leaf into its reported model attempts. `None` is an
/// unknown model, distinct from a real model named `(unattributed)`.
fn expanded_outcomes(node: &Node) -> Vec<(Option<String>, AttemptUsageOutcome)> {
    let Some(chain) = &node.chain else {
        return vec![(
            Some(node.model.clone()),
            node.outcome
                .clone()
                .unwrap_or(AttemptUsageOutcome::OutcomeUnknown { observed: None }),
        )];
    };

    let mut attempts = Vec::with_capacity(chain.prior.len() + 1);
    for entry in &chain.prior {
        let mut outcome = classify_usage(entry.usage.clone());
        if node.string_usage_eligible
            && !entry.well_formed
            && let Some(observed) = &entry.usage
        {
            outcome = AttemptUsageOutcome::Invalid {
                observed: observed.clone(),
                reason: InvalidUsageReason::MalformedUsage,
            };
        } else if entry.model.is_none() || !entry.well_formed {
            outcome = match outcome {
                AttemptUsageOutcome::Complete(usage) => AttemptUsageOutcome::OutcomeUnknown {
                    observed: Some(usage),
                },
                other => other,
            };
        }
        if !node.finalized {
            outcome = AttemptUsageOutcome::OutcomeUnknown {
                observed: outcome.observed_usage(),
            };
        }
        attempts.push((entry.model.clone(), outcome));
    }

    let mut terminal = if node.string_usage_eligible
        && chain.terminal.malformed
        && let Some(observed) = &chain.terminal.observation
    {
        AttemptUsageOutcome::Invalid {
            observed: observed.clone(),
            reason: InvalidUsageReason::MalformedUsage,
        }
    } else if chain.terminal.settled {
        node.outcome
            .clone()
            .unwrap_or(AttemptUsageOutcome::OutcomeUnknown { observed: None })
    } else if node.string_usage_eligible
        && let Some(AttemptUsageOutcome::Invalid { observed, reason }) = &node.outcome
    {
        // Ambiguous model attribution does not erase a malformed-wire
        // diagnosis already established by the decoder.
        AttemptUsageOutcome::Invalid {
            observed: chain
                .terminal
                .observation
                .as_ref()
                .unwrap_or(observed)
                .clone(),
            reason: *reason,
        }
    } else {
        match classify_usage(chain.terminal.observation.clone()) {
            AttemptUsageOutcome::Complete(usage) => AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(usage),
            },
            other => other,
        }
    };
    if !node.finalized {
        // A complete body may have been decoded before the dispatch lease was
        // dropped. Preserve its valid terminal observation as a lower bound.
        let observed = terminal.observed_usage().or_else(|| {
            if chain.terminal.settled {
                match classify_usage(chain.terminal.observation.clone()) {
                    AttemptUsageOutcome::Complete(usage) => Some(usage),
                    _ => None,
                }
            } else {
                None
            }
        });
        terminal = AttemptUsageOutcome::OutcomeUnknown { observed };
    }
    attempts.push((chain.terminal.model.clone(), terminal));
    attempts
}

#[derive(Debug, Default)]
struct CollectorState {
    closed: bool,
    logical_success: bool,
    capture_string_usage: bool,
    nodes: Vec<Node>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CallAccountingCollector {
    state: Arc<Mutex<CollectorState>>,
}

impl CallAccountingCollector {
    pub(crate) fn with_string_usage() -> Self {
        let collector = Self::default();
        collector.state.lock().capture_string_usage = true;
        collector
    }

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
                .find(|node| node.finalized && !node.composite && !node.suppressed)
                .map(|node| (node.provider_ref.clone(), node.model.clone()))
        } else {
            None
        };
        let attempts = nodes
            .iter()
            .filter(|node| !node.composite && !node.suppressed)
            .flat_map(|node| {
                expanded_outcomes(node)
                    .into_iter()
                    .map(move |(model, outcome)| {
                        let model_attributed = model.is_some();
                        AccountedAttempt::new(
                            node.provider_ref.clone(),
                            model.unwrap_or_else(|| UNATTRIBUTED_MODEL.to_string()),
                            model_attributed,
                            outcome,
                        )
                    })
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
    /// Only a String dispatch poll may opt its own node into observation.
    pub(crate) fn enable_string_usage(&self) {
        let mut state = self.collector.lock();
        if state.closed || !state.capture_string_usage {
            return;
        }
        if let Some(node) = state.nodes.get_mut(self.node.0)
            && !node.finalized
        {
            node.string_usage_eligible = true;
        }
    }

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
                    suppressed: false,
                    finalized: false,
                    string_usage_eligible: false,
                    outcome: None,
                    chain: None,
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

    pub(crate) fn finish_string_response(&self) {
        let mut state = self.collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state.nodes.get_mut(self.node.0) {
            if !node.string_usage_eligible || node.outcome.is_none() {
                node.outcome = Some(AttemptUsageOutcome::Missing);
            }
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
            if node.string_usage_eligible
                && matches!(node.outcome, Some(AttemptUsageOutcome::Invalid { .. }))
            {
                // A rejected String response can still contain an invalid
                // usage observation with trustworthy independent dimensions.
                node.finalized = true;
                return;
            }
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

/// Record an observation only on the currently polled eligible physical leaf.
pub(crate) fn observe_string_outcome(outcome: AttemptUsageOutcome) {
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
        let Some(node) = node else { return };
        let mut state = collector.lock();
        if state.closed {
            return;
        }
        if let Some(node) = state.nodes.get_mut(node.0)
            && node.string_usage_eligible
            && !node.composite
            && !node.suppressed
            && !node.finalized
        {
            node.outcome = Some(outcome);
        }
    });
}

pub(crate) fn observe_string_usage(usage: Option<TokenUsage>) {
    if usage.is_some() {
        observe_string_outcome(classify_usage(usage));
    }
}

pub(crate) fn observe_malformed_string_usage(observed: TokenUsage) {
    observe_invalid_string_usage(observed, InvalidUsageReason::MalformedUsage);
}

pub(crate) fn observe_invalid_string_usage(observed: TokenUsage, reason: InvalidUsageReason) {
    observe_string_outcome(AttemptUsageOutcome::Invalid { observed, reason });
}

pub(crate) fn string_usage_observation_enabled() -> bool {
    let mut eligible = false;
    with_current_open_leaf(|node| eligible = node.string_usage_eligible);
    eligible
}

/// A wire owner can recover independent counts after strict JSON decoding
/// rejects a response. This is not a second response parser or a retry.
pub(crate) trait ObserveRejectedStringWire {
    fn observe_rejected_usage(wire: &serde_json::Value);
}

pub(crate) fn observe_rejected_string_wire<T: ObserveRejectedStringWire>(body: &[u8]) {
    if string_usage_observation_enabled()
        && let Ok(wire) = serde_json::from_slice(body)
    {
        T::observe_rejected_usage(&wire);
    }
}

pub(crate) fn observe_completed_string_wire_rejection<T: ObserveRejectedStringWire>(body: &[u8]) {
    observe_rejected_string_wire::<T>(body);
    finish_current_string_attempt_after_completed_response();
}

pub(crate) struct StringWireResponse<T>(pub(crate) T);

impl<'de, T> Deserialize<'de> for StringWireResponse<T>
where
    T: DeserializeOwned + ObserveRejectedStringWire,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !string_usage_observation_enabled() {
            return T::deserialize(deserializer).map(Self);
        }
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        let decoded = serde_json::from_str::<T>(raw.get());
        if decoded.is_err() {
            observe_rejected_string_wire::<T>(raw.get().as_bytes());
        }
        decoded.map(Self).map_err(serde::de::Error::custom)
    }
}

/// Decode the three legacy OpenAI-style String usage objects independently so
/// a bad optional field cannot break text parsing or erase valid dimensions.
/// As with the structured response, a null optional field is absent, not zero.
pub(crate) fn observe_raw_string_usage(raw: Option<&serde_json::Value>, cached: bool) {
    observe_string_usage_fields(
        raw,
        "prompt_tokens",
        "completion_tokens",
        cached.then_some("prompt_tokens_details"),
    );
}

pub(crate) fn observe_responses_string_usage(raw: Option<&serde_json::Value>) {
    observe_string_usage_fields(
        raw,
        "input_tokens",
        "output_tokens",
        Some("input_tokens_details"),
    );
}

pub(crate) fn observe_string_usage_fields(
    raw: Option<&serde_json::Value>,
    input_field: &str,
    output_field: &str,
    cache_field: Option<&str>,
) {
    if let Some(observation) =
        project_string_usage_fields(raw, input_field, output_field, cache_field)
    {
        observe_string_outcome(observation);
    }
}

pub(crate) fn project_string_usage_fields(
    raw: Option<&serde_json::Value>,
    input_field: &str,
    output_field: &str,
    cache_field: Option<&str>,
) -> Option<AttemptUsageOutcome> {
    let raw = raw.filter(|value| !value.is_null())?;
    fn number(value: Option<&serde_json::Value>, malformed: &mut bool) -> Option<u64> {
        match value {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => match value.as_u64() {
                Some(n) => Some(n),
                None => {
                    *malformed = true;
                    None
                }
            },
        }
    }
    let mut malformed = false;
    let object = raw.as_object();
    let input = number(object.and_then(|o| o.get(input_field)), &mut malformed);
    let output = number(object.and_then(|o| o.get(output_field)), &mut malformed);
    let cache = match cache_field.and_then(|key| object.and_then(|o| o.get(key))) {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Object(details)) => {
            number(details.get("cached_tokens"), &mut malformed)
        }
        Some(_) => {
            malformed = true;
            None
        }
    };
    if object.is_none() {
        malformed = true;
    }
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cache,
        cache_creation_input_tokens: None,
    };
    Some(if malformed {
        AttemptUsageOutcome::Invalid {
            observed: usage,
            reason: InvalidUsageReason::MalformedUsage,
        }
    } else {
        classify_usage(Some(usage))
    })
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

/// Suppress the currently polled dispatch attempt so synthetic no-replay
/// returns (such as unwrapped refusal replays) do not bill a second physical attempt.
pub(crate) fn suppress_current_attempt() {
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
            current.suppressed = true;
            current.finalized = true;
            current.outcome = None;
        }
    });
}

/// Attach a decoded response's usage chain to the currently polled leaf.
/// Only the wire owner may call this, after it has received the entire body.
/// The first attachment wins; calls outside an open leaf are harmless.
pub(crate) fn attach_current_attempt_usage_chain(chain: UsageChain) {
    with_current_open_leaf(|node| {
        if node.chain.is_none() {
            node.chain = Some(chain);
        }
    });
}

pub(crate) fn attach_current_string_attempt_usage_chain(chain: UsageChain) {
    with_current_open_leaf(|node| {
        if node.string_usage_eligible && node.chain.is_none() {
            node.chain = Some(chain);
        }
    });
}

pub(crate) fn finish_current_string_attempt_after_completed_response() {
    with_current_open_leaf(|node| {
        if node.string_usage_eligible {
            node.outcome.get_or_insert(AttemptUsageOutcome::Missing);
            node.finalized = true;
        }
    });
}

/// Finalize a complete wire response that the adapter returns as an error.
pub(crate) fn finish_current_attempt_after_completed_response(usage: Option<TokenUsage>) {
    with_current_open_leaf(|node| {
        node.outcome = Some(classify_usage(usage));
        node.finalized = true;
    });
}

fn with_current_open_leaf(action: impl FnOnce(&mut Node)) {
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
            && !current.composite
            && !current.suppressed
            && !current.finalized
        {
            action(current);
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
                if node.composite || node.suppressed {
                    continue;
                }
                for (_, outcome) in expanded_outcomes(node) {
                    let usage = match outcome {
                        AttemptUsageOutcome::Complete(usage)
                        | AttemptUsageOutcome::OutcomeUnknown {
                            observed: Some(usage),
                        } => usage,
                        _ => continue,
                    };
                    let input = usage.input_tokens?;
                    let output = usage.output_tokens?;
                    let cached = usage.cached_input_tokens.unwrap_or(0);
                    let creation = usage.cache_creation_input_tokens.unwrap_or(0);
                    let next = TokenUsage {
                        input_tokens: Some(input),
                        output_tokens: Some(output),
                        cached_input_tokens: Some(cached),
                        cache_creation_input_tokens: Some(creation),
                    };
                    match &mut total {
                        Some(total) => {
                            total.input_tokens = total.input_tokens?.checked_add(input);
                            total.output_tokens = total.output_tokens?.checked_add(output);
                            total.cached_input_tokens =
                                total.cached_input_tokens?.checked_add(cached);
                            total.cache_creation_input_tokens =
                                total.cache_creation_input_tokens?.checked_add(creation);
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

pub(crate) fn classify_usage(usage: Option<TokenUsage>) -> AttemptUsageOutcome {
    let Some(usage) = usage else {
        return AttemptUsageOutcome::Missing;
    };
    if usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cached_input_tokens.is_none()
        && usage.cache_creation_input_tokens.is_none()
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
        .unwrap_or(0)
        .checked_add(usage.cache_creation_input_tokens.unwrap_or(0))
        .is_none_or(|cached| cached > input)
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

    fn string_test_usage() -> TokenUsage {
        TokenUsage {
            input_tokens: Some(8),
            output_tokens: Some(3),
            cached_input_tokens: Some(1),
            cache_creation_input_tokens: None,
        }
    }

    #[tokio::test]
    async fn string_observation_is_opt_in_and_uses_current_leaf() {
        for enabled in [false, true] {
            let collector = if enabled {
                CallAccountingCollector::with_string_usage()
            } else {
                CallAccountingCollector::default()
            };
            collector
                .scope(async {
                    let lease = AttemptLease::begin("p".into(), "m".into()).unwrap();
                    lease.enable_string_usage();
                    lease.poll_scope(|| observe_string_usage(Some(string_test_usage())));
                    lease.finish_string_response();
                })
                .await;
            let (attempts, _) = collector.close();
            if enabled {
                assert!(
                    matches!(attempts[0].outcome(), AttemptUsageOutcome::Complete(usage) if usage.input_tokens == Some(8) && usage.output_tokens == Some(3))
                );
            } else {
                assert!(matches!(
                    attempts[0].outcome(),
                    AttemptUsageOutcome::Missing
                ));
            }
        }
    }

    #[tokio::test]
    async fn string_invalid_unknown_drop_and_late_observation() {
        let collector = CallAccountingCollector::with_string_usage();
        collector
            .scope(async {
                let invalid = AttemptLease::begin("p".into(), "invalid".into()).unwrap();
                invalid.enable_string_usage();
                invalid.poll_scope(|| observe_malformed_string_usage(string_test_usage()));
                invalid.finish_string_response();
                invalid.poll_scope(|| observe_string_usage(Some(string_test_usage())));
                let failed = AttemptLease::begin("p".into(), "failed".into()).unwrap();
                failed.enable_string_usage();
                failed.poll_scope(|| observe_string_usage(Some(string_test_usage())));
                failed.finish_error_with_usage(None);
                let dropped = AttemptLease::begin("p".into(), "dropped".into()).unwrap();
                dropped.enable_string_usage();
                dropped.poll_scope(|| observe_string_usage(Some(string_test_usage())));
                drop(dropped);
            })
            .await;
        let (attempts, _) = collector.close();
        assert!(matches!(
            attempts[0].outcome(),
            AttemptUsageOutcome::Invalid {
                reason: InvalidUsageReason::MalformedUsage,
                ..
            }
        ));
        for attempt in &attempts[1..] {
            assert!(
                matches!(attempt.outcome(), AttemptUsageOutcome::OutcomeUnknown { observed: Some(usage) } if usage.input_tokens == Some(8))
            );
        }
        observe_string_usage(Some(string_test_usage()));
        assert!(collector.close().0.is_empty());
    }

    #[tokio::test]
    async fn raw_string_usage_preserves_partial_malformed_dimensions() {
        let collector = CallAccountingCollector::with_string_usage();
        collector
            .scope(async {
                let lease = AttemptLease::begin("p".into(), "m".into()).unwrap();
                lease.enable_string_usage();
                lease.poll_scope(|| {
                    observe_raw_string_usage(
                        Some(&serde_json::json!({
                            "prompt_tokens": 8, "completion_tokens": "bad",
                            "prompt_tokens_details": {"cached_tokens": 2}, "ignored": []
                        })),
                        true,
                    )
                });
                lease.finish_string_response();
            })
            .await;
        let (attempts, _) = collector.close();
        assert!(
            matches!(attempts[0].outcome(), AttemptUsageOutcome::Invalid {
            observed, reason: InvalidUsageReason::MalformedUsage
        } if observed.input_tokens == Some(8) && observed.output_tokens.is_none()
            && observed.cached_input_tokens == Some(2))
        );
    }

    #[tokio::test]
    async fn raw_string_usage_distinguishes_absent_partial_invalid_and_zero() {
        let cases = [
            (None, 0),
            (Some(serde_json::Value::Null), 0),
            (Some(serde_json::json!({"prompt_tokens": 8})), 1),
            (
                Some(serde_json::json!({"prompt_tokens": -1, "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": "8", "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": 3.5, "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": 1.8e19, "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": {}, "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": [], "completion_tokens": 3})),
                2,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": 0, "completion_tokens": 0})),
                3,
            ),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 9}}),
                ),
                4,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": u64::MAX, "completion_tokens": 1})),
                5,
            ),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": null}),
                ),
                6,
            ),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": null}}),
                ),
                6,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": null, "completion_tokens": 3})),
                7,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": 8, "completion_tokens": null})),
                8,
            ),
            (
                Some(serde_json::json!({"prompt_tokens": null, "completion_tokens": null})),
                0,
            ),
            (Some(serde_json::json!({})), 0),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": []}),
                ),
                9,
            ),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": "bad"}),
                ),
                9,
            ),
            (
                Some(
                    serde_json::json!({"prompt_tokens": 8, "completion_tokens": 3, "prompt_tokens_details": 7}),
                ),
                9,
            ),
        ];
        for (raw, expected) in cases {
            let collector = CallAccountingCollector::with_string_usage();
            collector
                .scope(async {
                    let lease = AttemptLease::begin("p".into(), "m".into()).unwrap();
                    lease.enable_string_usage();
                    lease.poll_scope(|| observe_raw_string_usage(raw.as_ref(), true));
                    lease.finish_string_response();
                })
                .await;
            let (attempts, _) = collector.close();
            let outcome = attempts[0].outcome();
            assert!(match expected {
                0 => matches!(outcome, AttemptUsageOutcome::Missing),
                1 => matches!(
                    outcome,
                    AttemptUsageOutcome::Invalid {
                        reason: InvalidUsageReason::MissingOutput,
                        ..
                    }
                ),
                2 =>
                    matches!(outcome, AttemptUsageOutcome::Invalid { reason: InvalidUsageReason::MalformedUsage, observed } if observed.output_tokens == Some(3)),
                3 =>
                    matches!(outcome, AttemptUsageOutcome::Complete(usage) if usage.input_tokens == Some(0) && usage.output_tokens == Some(0)),
                4 => matches!(
                    outcome,
                    AttemptUsageOutcome::Invalid {
                        reason: InvalidUsageReason::CachedInputExceedsInput,
                        ..
                    }
                ),
                5 => matches!(
                    outcome,
                    AttemptUsageOutcome::Invalid {
                        reason: InvalidUsageReason::TotalOverflow,
                        ..
                    }
                ),
                6 => matches!(outcome, AttemptUsageOutcome::Complete(usage)
                    if usage.input_tokens == Some(8) && usage.output_tokens == Some(3)
                        && usage.cached_input_tokens.is_none()),
                7 => matches!(outcome, AttemptUsageOutcome::Invalid {
                    reason: InvalidUsageReason::MissingInput, observed
                } if observed.output_tokens == Some(3)),
                8 => matches!(outcome, AttemptUsageOutcome::Invalid {
                    reason: InvalidUsageReason::MissingOutput, observed
                } if observed.input_tokens == Some(8)),
                9 => matches!(outcome, AttemptUsageOutcome::Invalid {
                    reason: InvalidUsageReason::MalformedUsage, observed
                } if observed.input_tokens == Some(8) && observed.output_tokens == Some(3)),
                _ => false,
            });
        }
    }

    #[tokio::test]
    async fn string_observer_ignores_composite_suppressed_and_closed_nodes() {
        let collector = CallAccountingCollector::with_string_usage();
        collector
            .scope(async {
                observe_string_usage(Some(string_test_usage())); // no current poll
                let structured = AttemptLease::begin("p".into(), "structured".into()).unwrap();
                structured.poll_scope(|| observe_string_usage(Some(string_test_usage())));
                structured.finish_string_response();
                let composite = AttemptLease::begin("p".into(), "composite".into()).unwrap();
                composite.enable_string_usage();
                composite.poll_scope(|| {
                    mark_current_composite();
                    observe_string_usage(Some(string_test_usage()));
                });
                composite.finish_string_response();
                let suppressed = AttemptLease::begin("p".into(), "suppressed".into()).unwrap();
                suppressed.enable_string_usage();
                suppressed.poll_scope(|| {
                    suppress_current_attempt();
                    observe_string_usage(Some(string_test_usage()));
                });
                let closed = AttemptLease::begin("p".into(), "closed".into()).unwrap();
                closed.enable_string_usage();
                let (attempts, _) = collector.close();
                assert_eq!(attempts.len(), 2);
                assert!(matches!(
                    attempts[0].outcome(),
                    AttemptUsageOutcome::Missing
                ));
                assert!(matches!(
                    attempts[1].outcome(),
                    AttemptUsageOutcome::OutcomeUnknown { observed: None }
                ));
                closed.poll_scope(|| observe_string_usage(Some(string_test_usage())));
            })
            .await;
        assert!(collector.close().0.is_empty());
    }

    fn chain_usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_input_tokens: None,
            cache_creation_input_tokens: None,
        }
    }

    fn test_chain() -> UsageChain {
        UsageChain {
            prior: vec![
                IterationRecord {
                    model: Some(UNATTRIBUTED_MODEL.to_string()),
                    usage: Some(chain_usage(10, 1)),
                    well_formed: true,
                },
                IterationRecord {
                    model: None,
                    usage: Some(chain_usage(20, 2)),
                    well_formed: true,
                },
            ],
            terminal: TerminalRecord {
                model: Some(UNATTRIBUTED_MODEL.to_string()),
                settled: true,
                malformed: false,
                observation: Some(chain_usage(30, 3)),
            },
        }
    }

    #[tokio::test]
    async fn chain_expansion_preserves_order_attribution_and_billable_total() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                let lease = AttemptLease::begin("provider".into(), "route".into()).unwrap();
                lease.poll_scope(|| {
                    attach_current_attempt_usage_chain(test_chain());
                    // A later attachment must not replace the wire owner's first one.
                    attach_current_attempt_usage_chain(UsageChain {
                        prior: Vec::new(),
                        terminal: TerminalRecord {
                            model: None,
                            settled: false,
                            malformed: false,
                            observation: None,
                        },
                    });
                    finish_current_attempt_after_completed_response(Some(chain_usage(30, 3)));
                    let total = current_billable_usage().expect("billable chain");
                    assert_eq!(
                        (total.input_tokens, total.output_tokens),
                        (Some(60), Some(6))
                    );
                });
                collector.mark_logical_success();
            })
            .await;

        let (attempts, route) = collector.close();
        assert_eq!(route, Some(("provider".into(), "route".into())));
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts.iter().map(|a| a.model()).collect::<Vec<_>>(),
            vec![UNATTRIBUTED_MODEL; 3]
        );
        assert_eq!(
            attempts
                .iter()
                .map(|a| a.is_model_attributed())
                .collect::<Vec<_>>(),
            vec![true, false, true]
        );
        assert!(matches!(
            attempts[0].outcome(),
            AttemptUsageOutcome::Complete(_)
        ));
        assert!(matches!(
            attempts[1].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: Some(_) }
        ));
        assert!(matches!(
            attempts[2].outcome(),
            AttemptUsageOutcome::Complete(_)
        ));
        let report = super::super::AccountedCallReport::new(None).with_attempts(attempts, route);
        assert_eq!(
            report
                .rejected_attempts()
                .iter()
                .map(|a| a.is_model_attributed())
                .collect::<Vec<_>>(),
            vec![true, false]
        );
    }

    #[tokio::test]
    async fn dropped_chain_keeps_valid_decoded_usage_as_unknown_lower_bounds() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                let lease = AttemptLease::begin("provider".into(), "route".into()).unwrap();
                lease.poll_scope(|| attach_current_attempt_usage_chain(test_chain()));
                // Dropping the unfinished lease simulates cancellation after decode.
            })
            .await;
        let (attempts, route) = collector.close();
        assert!(route.is_none());
        assert_eq!(attempts.len(), 3);
        for (attempt, input) in attempts.iter().zip([10, 20, 30]) {
            assert!(
                matches!(attempt.outcome(), AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage { input_tokens: Some(actual), .. })
            } if *actual == input)
            );
        }
    }

    #[tokio::test]
    async fn semantic_rejection_downgrades_only_the_settled_terminal() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                let lease = AttemptLease::begin("provider".into(), "route".into()).unwrap();
                lease.poll_scope(|| {
                    attach_current_attempt_usage_chain(test_chain());
                    finish_current_attempt_after_completed_response(Some(chain_usage(30, 3)));
                    record_stream_semantic_rejection_usage(chain_usage(30, 3));
                });
            })
            .await;
        let (attempts, route) = collector.close();
        assert!(route.is_none());
        assert!(matches!(
            attempts[0].outcome(),
            AttemptUsageOutcome::Complete(_)
        ));
        assert!(matches!(
            attempts[1].outcome(),
            AttemptUsageOutcome::OutcomeUnknown { observed: Some(_) }
        ));
        assert!(matches!(
            attempts[2].outcome(),
            AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(3),
                    ..
                })
            }
        ));
    }

    #[tokio::test]
    async fn chain_attachment_ignores_suppressed_composite_and_finalized_leaves() {
        let collector = CallAccountingCollector::default();
        collector
            .scope(async {
                for state in ["suppressed", "composite", "finalized"] {
                    let lease = AttemptLease::begin("provider".into(), state.into()).unwrap();
                    lease.poll_scope(|| {
                        match state {
                            "suppressed" => suppress_current_attempt(),
                            "composite" => mark_current_composite(),
                            "finalized" => finish_current_attempt_after_completed_response(Some(
                                chain_usage(7, 1),
                            )),
                            _ => unreachable!(),
                        }
                        attach_current_attempt_usage_chain(test_chain());
                    });
                }
            })
            .await;
        let (attempts, _) = collector.close();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].model(), "finalized");
        assert!(matches!(
            attempts[0].outcome(),
            AttemptUsageOutcome::Complete(_)
        ));
        attach_current_attempt_usage_chain(test_chain()); // No scoped leaf: no-op.
    }

    #[test]
    fn usage_classification_preserves_missing_zero_and_invalid_distinctions() {
        assert!(matches!(classify_usage(None), AttemptUsageOutcome::Missing));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: None,
                output_tokens: None,
                cached_input_tokens: None,
                cache_creation_input_tokens: None,
            })),
            AttemptUsageOutcome::Missing
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: Some(0),
                output_tokens: Some(0),
                cached_input_tokens: Some(0),
                cache_creation_input_tokens: None,
            })),
            AttemptUsageOutcome::Complete(_)
        ));
        assert!(matches!(
            classify_usage(Some(TokenUsage {
                input_tokens: None,
                output_tokens: Some(1),
                cached_input_tokens: None,
                cache_creation_input_tokens: None,
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
                cache_creation_input_tokens: None,
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
                cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
        });

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                    cached_input_tokens: Some(1),
                    cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
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
                    cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
        });
        lease.observe_stream_usage(TokenUsage {
            input_tokens: Some(15),
            output_tokens: Some(3),
            cached_input_tokens: Some(2),
            cache_creation_input_tokens: None,
        });

        let (report, _) = collector.close();
        assert!(matches!(
            report[0].outcome(),
            AttemptUsageOutcome::OutcomeUnknown {
                observed: Some(TokenUsage {
                    input_tokens: Some(15),
                    output_tokens: Some(3),
                    cached_input_tokens: Some(2),
                    cache_creation_input_tokens: None,
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
            cache_creation_input_tokens: None,
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
                cache_creation_input_tokens: None,
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
