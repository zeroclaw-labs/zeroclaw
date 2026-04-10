//! Conversation recovery: error classification and retry prompts.
//!
//! When a tool-call loop fails with a recoverable error (e.g. the model narrates
//! an action instead of emitting a tool call), the channel handler retries the
//! invocation with escalating prompt strategies before surfacing the error to
//! the user.

use crate::providers::ProviderCapabilityError;

/// Whether an agent loop error can be retried with a different strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorRecoverability {
    /// Model protocol failure — retry with escalating prompt strategy.
    /// Examples: deferred action without tool call, tool-call parse failure.
    Recoverable,

    /// Infrastructure hiccup — the recovery wrapper does not handle these
    /// directly (they have dedicated branches in the channel handler), but
    /// the classifier exposes the category for future use.
    Transient,

    /// Hard stop — do not retry under any circumstances.
    /// Examples: cost limit, emergency stop, security violation, cancellation.
    Fatal,
}

/// Classify an agent-loop error into a recoverability category.
///
/// The channel handler at `channels/mod.rs` already branches on specific error
/// types (context overflow, iteration limit, cancellation).  This classifier
/// covers the remaining errors that reach the generic `else` branch, deciding
/// whether a retry-around-loop is worthwhile.
pub fn classify_error(err: &anyhow::Error) -> ErrorRecoverability {
    // Fatal: framework-level hard stops — never retry.
    if crate::agent::loop_::is_tool_loop_cancelled(err) {
        return ErrorRecoverability::Fatal;
    }
    if crate::agent::loop_::is_tool_iteration_limit_error(err) {
        return ErrorRecoverability::Fatal;
    }
    if err.downcast_ref::<ProviderCapabilityError>().is_some() {
        return ErrorRecoverability::Fatal;
    }

    let text = err.to_string();
    let lower = text.to_lowercase();

    // Fatal: security / cost / estop.
    if lower.contains("security policy")
        || lower.contains("cost limit")
        || lower.contains("emergency stop")
        || lower.contains("estop")
        || lower.contains("loop detection hard-stop")
    {
        return ErrorRecoverability::Fatal;
    }

    // Recoverable: model protocol failures where a retry with a different
    // prompt strategy has a reasonable chance of succeeding.
    if lower.contains("deferred action without emitting a tool call")
        || lower.contains("tool-call parse")
        || lower.contains("no valid tool call was emitted")
    {
        return ErrorRecoverability::Recoverable;
    }

    // Transient: provider / infrastructure issues.  The channel handler has
    // dedicated branches for context-window overflow and timeouts already, so
    // these rarely reach us.  Classify them for completeness.
    if lower.contains("503")
        || lower.contains("rate limit")
        || lower.contains("timed out")
        || lower.contains("connection refused")
    {
        return ErrorRecoverability::Transient;
    }

    // Default: don't retry unknown errors.
    ErrorRecoverability::Fatal
}

// ── Recovery Prompts ──────────────────────────────────────────────────────

/// Injected as a user message before the second attempt.  Firm but not
/// aggressive — the model gets one more clean shot with explicit instructions.
pub const RECOVERY_PROMPT_ATTEMPT_2: &str = "\
RECOVERY: Your previous attempt described performing actions in natural language \
without actually calling any tools. This is not acceptable — the user received \
no result.\n\n\
Rules for this attempt:\n\
1. If you need to use a tool, you MUST emit a tool call. Describing what you \
   would do is not the same as doing it.\n\
2. If you have enough information to answer without tools, provide the final \
   answer directly. Do not reference actions you did not actually perform.\n\
3. Do not apologize or explain the retry. Just do the work.";

/// Injected before the final attempt.  Context is compressed and the original
/// user request is re-stated explicitly.
pub fn recovery_prompt_attempt_final(original_user_message: &str) -> String {
    format!(
        "RECOVERY (final attempt): Previous attempts failed to produce valid tool calls. \
         The conversation history has been compacted.\n\n\
         The user's original request was:\n---\n{original_user_message}\n---\n\n\
         Respond to this request now. Use tools if needed (emit tool calls), \
         or provide a direct answer. This is your last attempt."
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_error(msg: &str) -> anyhow::Error {
        anyhow::anyhow!("{msg}")
    }

    #[test]
    fn deferred_action_is_recoverable() {
        let err =
            make_error("Model deferred action without emitting a tool call after retry; refusing to return unverified completion.");
        assert_eq!(classify_error(&err), ErrorRecoverability::Recoverable);
    }

    #[test]
    fn tool_call_parse_is_recoverable() {
        let err = make_error("tool-call parse failure in assistant response");
        assert_eq!(classify_error(&err), ErrorRecoverability::Recoverable);
    }

    #[test]
    fn security_policy_is_fatal() {
        let err = make_error("Command blocked by security policy: forbidden path");
        assert_eq!(classify_error(&err), ErrorRecoverability::Fatal);
    }

    #[test]
    fn cost_limit_is_fatal() {
        let err = make_error("cost limit exceeded for daily budget");
        assert_eq!(classify_error(&err), ErrorRecoverability::Fatal);
    }

    #[test]
    fn provider_503_is_transient() {
        let err = make_error("503 Service Unavailable from upstream provider");
        assert_eq!(classify_error(&err), ErrorRecoverability::Transient);
    }

    #[test]
    fn rate_limit_is_transient() {
        let err = make_error("rate limit exceeded, retry after 30s");
        assert_eq!(classify_error(&err), ErrorRecoverability::Transient);
    }

    #[test]
    fn unknown_error_is_fatal() {
        let err = make_error("something completely unexpected happened");
        assert_eq!(classify_error(&err), ErrorRecoverability::Fatal);
    }

    #[test]
    fn capability_error_is_fatal() {
        let err = anyhow::Error::new(ProviderCapabilityError {
            provider: "test".into(),
            capability: "vision".into(),
            message: "not supported".into(),
        });
        assert_eq!(classify_error(&err), ErrorRecoverability::Fatal);
    }

    #[test]
    fn recovery_prompt_final_includes_user_message() {
        let prompt = recovery_prompt_attempt_final("Tell me about the McBarge");
        assert!(prompt.contains("Tell me about the McBarge"));
        assert!(prompt.contains("final attempt"));
    }
}
