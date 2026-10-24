//! The tool restriction a channel instance places on the work its turns start.
//!
//! A channel instance can exclude tools (`channels.<type>.<alias>.excluded_tools`).
//! The outer turn loop applies that list directly, but a turn can start further
//! loops: a delegate, a sub-agent, a manually run cron job. Those assemble
//! their own tool sets, so the restriction travels with the task instead, and
//! every tool loop on the task merges it into its excluded tools.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Tools that start work a task-carried restriction cannot follow: work that
/// runs later (cron, schedule, SOP steps), as another agent (peer, A2A,
/// external agent CLIs), or through another tool runner that checks only its
/// own policy (`execute_pipeline`). A restricted channel refuses them unless
/// the operator allows it with
/// `channels.restricted_channels_allow_detached_work`.
pub const DETACHED_WORK_TOOLS: &[&str] = &[
    "a2a_send",
    "browser_delegate",
    "claude_code",
    "claude_code_runner",
    "codex_cli",
    "cron_add",
    "cron_run",
    "cron_update",
    "execute_pipeline",
    "gemini_cli",
    "opencode_cli",
    "schedule",
    "send_message_to_peer",
    "sop_advance",
    "sop_approve",
    "sop_execute",
];

tokio::task_local! {
    /// Tool names excluded by the channel instance whose turn is running on
    /// this task. Tool calls run inline on the turn's task, so a tool that
    /// starts a nested loop runs it under the same restriction.
    static CHANNEL_TOOL_RESTRICTION: Arc<[String]>;
}

/// Run `future` with `excluded` as the channel tool restriction.
pub async fn with_channel_tool_restriction<T>(
    excluded: Arc<[String]>,
    future: impl Future<Output = T>,
) -> T {
    CHANNEL_TOOL_RESTRICTION.scope(excluded, future).await
}

/// Re-establish a captured restriction on a task that could not inherit it.
///
/// A task-local does not cross `spawn`, so a caller that hands work to a
/// background task captures [`active_channel_tool_restriction`] first and
/// restores it inside. `None` runs `future` unchanged.
///
/// The future is boxed at once: callers wrap large delegation futures right
/// before a spawn, and holding one more copy of that state on the stack
/// overflows it.
pub fn with_inherited_channel_tool_restriction<'a, T: 'a>(
    excluded: Option<Arc<[String]>>,
    future: impl Future<Output = T> + Send + 'a,
) -> Pin<Box<dyn Future<Output = T> + Send + 'a>> {
    let future = Box::pin(future);
    match excluded {
        Some(excluded) => Box::pin(CHANNEL_TOOL_RESTRICTION.scope(excluded, future)),
        None => future,
    }
}

/// The channel tool restriction this task is running under, if any.
#[must_use]
pub fn active_channel_tool_restriction() -> Option<Arc<[String]>> {
    CHANNEL_TOOL_RESTRICTION.try_with(Arc::clone).ok()
}

/// `excluded_tools` with the active channel restriction merged in, or `None`
/// when no restriction is active (the caller keeps its own slice).
#[must_use]
pub fn merge_active_channel_restriction(excluded_tools: &[String]) -> Option<Vec<String>> {
    let restriction = active_channel_tool_restriction()?;
    let mut merged = excluded_tools.to_vec();
    merged.extend(restriction.iter().cloned());
    merged.sort();
    merged.dedup();
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_restriction_outside_a_channel_turn() {
        assert!(active_channel_tool_restriction().is_none());
        assert!(merge_active_channel_restriction(&["a".into()]).is_none());
    }

    #[tokio::test]
    async fn a_restriction_merges_into_the_loop_exclusions() {
        let restriction: Arc<[String]> = vec!["shell".to_string()].into();
        let merged = with_channel_tool_restriction(restriction, async {
            merge_active_channel_restriction(&["browser".into(), "shell".into()])
        })
        .await;
        assert_eq!(
            merged,
            Some(vec!["browser".to_string(), "shell".to_string()])
        );
    }

    #[tokio::test]
    async fn a_captured_restriction_survives_a_spawn() {
        let restriction: Arc<[String]> = vec!["shell".to_string()].into();
        let seen = with_channel_tool_restriction(restriction, async {
            let captured = active_channel_tool_restriction();
            zeroclaw_spawn::spawn!(with_inherited_channel_tool_restriction(captured, async {
                active_channel_tool_restriction().map(|r| r.to_vec())
            }))
            .await
            .unwrap()
        })
        .await;
        assert_eq!(seen, Some(vec!["shell".to_string()]));
    }
}
