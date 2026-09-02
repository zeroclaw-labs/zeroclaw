//! The capability ceiling a turn imposes on everything it starts.
//!
//! A turn can exclude tools for reasons the agent's static configuration knows
//! nothing about — a skill declaring `blocked_tools_with_image` removes a tool
//! for the duration of an image turn, and only for that turn. That restriction
//! is enforced where the model's tool calls are dispatched, which is correct
//! for a direct call and insufficient for an indirect one: `delegate` and
//! `spawn_subagent` start *new* loops, and a child loop built from static
//! configuration has never heard of the parent's restriction. A model that
//! cannot call the tool directly could simply ask a child to.
//!
//! The ceiling closes that gap. [`run_tool_call_loop`] publishes its own
//! exclusions here for the duration of the loop, unioned with whatever ceiling
//! it inherited, and the nested-execution tools resolve it at call time and
//! fold it into the child's policy. Nesting can therefore only ever narrow
//! capability, never widen it, however deep it goes.
//!
//! This is a transport, not a second source of truth: the values come from the
//! caller that computed the turn's exclusions, and nothing writes here except
//! the loop that is already enforcing them.
//!
//! [`run_tool_call_loop`]: crate::agent::turn::run_tool_call_loop

use std::sync::Arc;

tokio::task_local! {
    /// Tools no execution started by the current turn may call, however deeply
    /// nested. Absent outside a turn, which reads as "no ceiling".
    static TURN_TOOL_CEILING: Arc<[String]>;
}

/// The tools the current turn forbids, including everything inherited from
/// enclosing turns.
///
/// Empty outside a scoped turn, which is the honest answer: with no turn there
/// is no turn-scoped restriction to apply. Static agent policy is enforced
/// separately and is unaffected by this.
pub fn current_tool_ceiling() -> Vec<String> {
    TURN_TOOL_CEILING
        .try_with(|ceiling| ceiling.to_vec())
        .unwrap_or_default()
}

/// Run `fut` with `additional` added to the current ceiling.
///
/// The union is taken against whatever is already in scope, so a child loop
/// that excludes nothing still inherits its parent's restrictions, and one
/// that excludes more adds to them. The result is immutable for the duration:
/// nothing inside `fut` can lower the ceiling it was given.
pub async fn with_tool_ceiling<F>(additional: &[String], fut: F) -> F::Output
where
    F: std::future::Future,
{
    let mut merged = current_tool_ceiling();
    for tool in additional {
        if !merged.iter().any(|existing| existing == tool) {
            merged.push(tool.clone());
        }
    }

    if merged.is_empty() {
        // Nothing to enforce; scoping an empty ceiling would only cost an
        // allocation per turn.
        return fut.await;
    }

    TURN_TOOL_CEILING.scope(Arc::from(merged), fut).await
}

/// Fold the current ceiling into a policy's excluded-tool list.
///
/// Used where a nested execution builds its child policy from static
/// configuration: the child keeps everything its own policy forbids and gains
/// everything the turn forbids.
pub fn apply_ceiling_to_excluded(policy_excluded: Option<&[String]>) -> Vec<String> {
    let mut merged: Vec<String> = policy_excluded.unwrap_or(&[]).to_vec();
    for tool in current_tool_ceiling() {
        if !merged.iter().any(|existing| existing == &tool) {
            merged.push(tool);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ceiling_is_empty_outside_a_turn() {
        assert!(current_tool_ceiling().is_empty());
    }

    #[tokio::test]
    async fn nesting_only_ever_narrows() {
        with_tool_ceiling(&["alpha".to_string()], async {
            assert_eq!(current_tool_ceiling(), vec!["alpha".to_string()]);

            // A nested scope that forbids nothing still inherits.
            with_tool_ceiling(&[], async {
                assert_eq!(current_tool_ceiling(), vec!["alpha".to_string()]);
            })
            .await;

            // One that forbids more adds to the ceiling rather than replacing it.
            with_tool_ceiling(&["beta".to_string()], async {
                assert_eq!(
                    current_tool_ceiling(),
                    vec!["alpha".to_string(), "beta".to_string()]
                );
            })
            .await;

            // Leaving the nested scope restores the enclosing ceiling; a child
            // cannot widen what its parent forbade.
            assert_eq!(current_tool_ceiling(), vec!["alpha".to_string()]);
        })
        .await;
    }

    #[tokio::test]
    async fn duplicate_entries_are_not_accumulated() {
        with_tool_ceiling(&["alpha".to_string()], async {
            with_tool_ceiling(&["alpha".to_string()], async {
                assert_eq!(current_tool_ceiling(), vec!["alpha".to_string()]);
            })
            .await;
        })
        .await;
    }

    #[tokio::test]
    async fn policy_exclusions_are_kept_alongside_the_ceiling() {
        with_tool_ceiling(&["blocked_by_turn".to_string()], async {
            let merged = apply_ceiling_to_excluded(Some(&["blocked_by_policy".to_string()]));
            assert_eq!(
                merged,
                vec![
                    "blocked_by_policy".to_string(),
                    "blocked_by_turn".to_string()
                ]
            );
        })
        .await;
    }
}
