//! Turn-loop control-flow outcomes: cancellation and model-switch errors.

use std::sync::{Arc, Mutex};

/// Callback type for checking if model has been switched during tool execution.
/// Returns Some((model_provider, model)) if a switch was requested, None otherwise.
pub type ModelSwitchCallback = Arc<Mutex<Option<(String, String)>>>;

tokio::task_local! {
    /// Pending model switch for one active tool loop. The loop owns this state;
    /// tools only borrow the current task-local handle while they execute.
    static MODEL_SWITCH_REQUEST: ModelSwitchCallback;
}

pub(crate) fn current_model_switch_state() -> anyhow::Result<ModelSwitchCallback> {
    MODEL_SWITCH_REQUEST.try_with(Arc::clone).map_err(|_| {
        anyhow::Error::msg("model_switch is only available inside an active agent turn")
    })
}

pub(crate) async fn scope_model_switch_state<F>(state: ModelSwitchCallback, future: F) -> F::Output
where
    F: std::future::Future,
{
    MODEL_SWITCH_REQUEST.scope(state, future).await
}

#[derive(Debug)]
pub struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

#[derive(Debug)]
pub(crate) struct StreamInterruptedAfterOutput {
    pub(crate) partial_text: String,
    pub(crate) message: String,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

impl std::fmt::Display for StreamInterruptedAfterOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamInterruptedAfterOutput {}

/// A no-output stream reached a provider-declared terminal incomplete state.
/// The candidate metadata is transient: it lets a composite provider continue
/// after the candidate that stopped instead of replaying it.
#[derive(Debug)]
pub(crate) struct StreamTerminalCompletion {
    pub(crate) failure: zeroclaw_api::model_provider::TerminalCompletionFailure,
    pub(crate) policy: zeroclaw_providers::TerminalCompletionPolicy,
    pub(crate) failed_candidate: Option<zeroclaw_api::model_provider::StreamProviderAttempt>,
}

impl std::fmt::Display for StreamTerminalCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(f)
    }
}

impl std::error::Error for StreamTerminalCompletion {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

/// A stream completed without a final response after the provider reported
/// tool work it had already executed. Replaying the request could repeat those
/// side effects, so this bypasses the normal non-streaming fallback.
#[derive(Debug)]
pub(crate) struct StreamPreExecutedToolsWithoutFinalResponse {
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

impl std::fmt::Display for StreamPreExecutedToolsWithoutFinalResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider stream ended after provider-executed tools without a final response")
    }
}

impl std::error::Error for StreamPreExecutedToolsWithoutFinalResponse {}

/// A completed stream that contains neither final text nor native tool calls.
/// Its reported usage survives the error boundary so retries and fallback do
/// not hide already-billed provider work from turn cost accounting.
#[derive(Debug)]
pub(crate) struct StreamSemanticEmptyCompletion {
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

impl std::fmt::Display for StreamSemanticEmptyCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider stream completed without final text or tool calls")
    }
}

impl std::error::Error for StreamSemanticEmptyCompletion {}

/// A stream failed before exposing output, after the provider reported usage.
/// Keep that usage through the recovery boundary so the fallback cannot hide
/// a billed failed attempt.
#[derive(Debug)]
pub(crate) struct StreamFailureWithoutOutput {
    pub(crate) message: String,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

impl std::fmt::Display for StreamFailureWithoutOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamFailureWithoutOutput {}

/// A non-streaming provider response that cannot complete a turn because it
/// exposes neither a final answer nor a tool call. Keep this typed through the
/// turn boundary so delivery adapters do not infer it from English diagnostics.
#[derive(Debug)]
pub(crate) struct SemanticEmptyTerminalCompletion;

impl std::fmt::Display for SemanticEmptyTerminalCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider completed without final text or tool calls")
    }
}

impl std::error::Error for SemanticEmptyTerminalCompletion {}

/// Whether a turn error has the canonical semantic-empty terminal reason.
pub fn is_semantic_empty_terminal_completion(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source.is::<SemanticEmptyTerminalCompletion>()
            || source.is::<StreamSemanticEmptyCompletion>()
            || source.is::<zeroclaw_providers::ReliableSemanticEmptyCompletion>()
    })
}

/// Render the canonical user-facing failure at a delivery boundary.
pub fn semantic_empty_terminal_completion_message(agent_name: Option<&str>) -> String {
    match agent_name {
        Some(agent_name) => crate::i18n::get_required_cli_string_with_args(
            "cli-delegate-error-invalid-semantic-completion",
            &[("agent_name", agent_name)],
        ),
        None => crate::i18n::get_required_cli_string("cli-agent-error-invalid-semantic-completion"),
    }
}

fn pre_executed_tools_without_final_response_message(agent_name: Option<&str>) -> String {
    match agent_name {
        Some(agent_name) => crate::i18n::get_required_cli_string_with_args(
            "cli-delegate-error-incomplete-after-provider-tools",
            &[("agent_name", agent_name)],
        ),
        None => {
            crate::i18n::get_required_cli_string("cli-agent-error-incomplete-after-provider-tools")
        }
    }
}

fn terminal_reason_message(
    reason: zeroclaw_api::model_provider::TerminalCompletionError,
    agent_name: Option<&str>,
) -> String {
    let (agent_key, delegate_key) = match reason {
        zeroclaw_api::model_provider::TerminalCompletionError::OutputTokenLimit => (
            "cli-agent-error-output-token-limit",
            "cli-delegate-error-output-token-limit",
        ),
        zeroclaw_api::model_provider::TerminalCompletionError::ContextWindow => (
            "cli-agent-error-context-window",
            "cli-delegate-error-context-window",
        ),
        zeroclaw_api::model_provider::TerminalCompletionError::PausedTurn => (
            "cli-agent-error-paused-turn",
            "cli-delegate-error-paused-turn",
        ),
        zeroclaw_api::model_provider::TerminalCompletionError::Refusal => {
            ("cli-agent-error-refusal", "cli-delegate-error-refusal")
        }
        zeroclaw_api::model_provider::TerminalCompletionError::InvalidTerminalReason => (
            "cli-agent-error-invalid-terminal-reason",
            "cli-delegate-error-invalid-terminal-reason",
        ),
    };

    match agent_name {
        Some(agent_name) => crate::i18n::get_required_cli_string_with_args(
            delegate_key,
            &[("agent_name", agent_name)],
        ),
        None => crate::i18n::get_required_cli_string(agent_key),
    }
}

/// Map typed terminal-delivery failures to their Fluent user-facing message.
pub fn terminal_completion_error_message(
    err: &anyhow::Error,
    agent_name: Option<&str>,
) -> Option<String> {
    if is_semantic_empty_terminal_completion(err) {
        return Some(semantic_empty_terminal_completion_message(agent_name));
    }
    if let Some(reason) = zeroclaw_api::model_provider::terminal_completion_error(err) {
        return Some(terminal_reason_message(reason, agent_name));
    }
    err.chain()
        .any(|source| source.is::<StreamPreExecutedToolsWithoutFinalResponse>())
        .then(|| pre_executed_tools_without_final_response_message(agent_name))
}

#[derive(Debug)]
pub(crate) struct StreamCancelledAfterOutput {
    pub(crate) partial_text: String,
    cause: ToolLoopCancelled,
}

impl StreamCancelledAfterOutput {
    pub(crate) fn new(partial_text: String) -> Self {
        Self {
            partial_text,
            cause: ToolLoopCancelled,
        }
    }
}

impl std::fmt::Display for StreamCancelledAfterOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled after streamed output")
    }
}

impl std::error::Error for StreamCancelledAfterOutput {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

#[derive(Debug)]
pub struct ModelSwitchRequested {
    pub model_provider: String,
    pub model: String,
}

impl std::fmt::Display for ModelSwitchRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "model switch requested to {} {}",
            self.model_provider, self.model
        )
    }
}

impl std::error::Error for ModelSwitchRequested {}

pub fn is_model_switch_requested(err: &anyhow::Error) -> Option<(String, String)> {
    err.chain()
        .filter_map(|source| source.downcast_ref::<ModelSwitchRequested>())
        .map(|e| (e.model_provider.clone(), e.model.clone()))
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_loop_cancelled_display() {
        let err = ToolLoopCancelled;
        assert_eq!(err.to_string(), "tool loop cancelled");
    }

    #[test]
    fn is_tool_loop_cancelled_direct() {
        let err = anyhow::Error::new(ToolLoopCancelled);
        assert!(is_tool_loop_cancelled(&err));
    }

    #[test]
    fn is_tool_loop_cancelled_unrelated_error_returns_false() {
        let err = anyhow::Error::msg("some other error");
        assert!(!is_tool_loop_cancelled(&err));
    }

    #[test]
    fn terminal_completion_diagnostics_are_stable_english() {
        let direct = SemanticEmptyTerminalCompletion;
        let stream = StreamSemanticEmptyCompletion { usage: None };
        let provider_tools = StreamPreExecutedToolsWithoutFinalResponse { usage: None };

        assert_eq!(
            direct.to_string(),
            "provider completed without final text or tool calls"
        );
        assert_eq!(
            stream.to_string(),
            "provider stream completed without final text or tool calls"
        );
        assert_eq!(
            provider_tools.to_string(),
            "provider stream ended after provider-executed tools without a final response"
        );
    }

    #[test]
    fn semantic_empty_cause_uses_the_fluent_delivery_projection() {
        let error = anyhow::Error::new(SemanticEmptyTerminalCompletion);
        assert!(is_semantic_empty_terminal_completion(&error));
        assert_eq!(
            terminal_completion_error_message(&error, None),
            Some("The model provider returned an invalid semantic completion.".to_string())
        );
    }

    #[test]
    fn native_terminal_reasons_use_direct_and_delegate_fluent_messages() {
        use zeroclaw_api::model_provider::{TerminalCompletionError, TerminalCompletionFailure};

        let cases = [
            (
                TerminalCompletionError::OutputTokenLimit,
                "The provider reached its output token limit before completing the response.",
                "Agent 'reviewer' failed: the provider reached its output token limit before completing the response.",
            ),
            (
                TerminalCompletionError::ContextWindow,
                "The provider reached its context window before completing the response.",
                "Agent 'reviewer' failed: the provider reached its context window before completing the response.",
            ),
            (
                TerminalCompletionError::PausedTurn,
                "The provider paused the turn before completing the response.",
                "Agent 'reviewer' failed: the provider paused the turn before completing the response.",
            ),
            (
                TerminalCompletionError::Refusal,
                "The provider refused before completing the response.",
                "Agent 'reviewer' failed: the provider refused before completing the response.",
            ),
            (
                TerminalCompletionError::InvalidTerminalReason,
                "The provider ended with an invalid terminal response state.",
                "Agent 'reviewer' failed: the provider ended with an invalid terminal response state.",
            ),
        ];

        for (reason, direct, delegate) in cases {
            let error = anyhow::Error::new(TerminalCompletionFailure::from(reason));

            assert_eq!(
                terminal_completion_error_message(&error, None).as_deref(),
                Some(direct)
            );
            assert_eq!(
                terminal_completion_error_message(&error, Some("reviewer")).as_deref(),
                Some(delegate)
            );
            assert_eq!(error.to_string(), reason.to_string());
        }
    }

    #[test]
    fn streamed_terminal_reason_uses_fluent_delivery_projection() {
        use zeroclaw_api::model_provider::{TerminalCompletionError, TerminalCompletionFailure};

        let error = anyhow::Error::new(StreamTerminalCompletion {
            failure: TerminalCompletionFailure::from(TerminalCompletionError::OutputTokenLimit),
            policy: zeroclaw_providers::default_terminal_policy(
                TerminalCompletionError::OutputTokenLimit,
            ),
            failed_candidate: None,
        });

        assert_eq!(
            terminal_completion_error_message(&error, None),
            Some(
                "The provider reached its output token limit before completing the response."
                    .to_string()
            )
        );
        assert_eq!(
            error.to_string(),
            "response incomplete: output token limit reached"
        );
    }

    #[test]
    fn disk_catalog_override_changes_delivery_not_the_diagnostic() {
        let error = anyhow::Error::new(SemanticEmptyTerminalCompletion);
        let disk_override =
            "cli-agent-error-invalid-semantic-completion = Réponse terminale invalide.\n";
        let delivered = crate::i18n::get_disk_override_cli_string_for_test(
            "fr",
            disk_override,
            "cli-agent-error-invalid-semantic-completion",
            &[],
        );

        assert_eq!(delivered, "Réponse terminale invalide.");
        assert_eq!(
            error.to_string(),
            "provider completed without final text or tool calls"
        );
    }

    #[test]
    fn stream_cancelled_after_output_display() {
        let e = StreamCancelledAfterOutput::new("partial text".to_string());
        assert_eq!(e.to_string(), "tool loop cancelled after streamed output");
        assert_eq!(e.partial_text, "partial text");
    }

    #[test]
    fn stream_cancelled_after_output_source_chains_to_tool_loop_cancelled() {
        use std::error::Error;
        let e = StreamCancelledAfterOutput::new(String::new());
        let source = e.source().expect("must have source");
        assert!(source.is::<ToolLoopCancelled>());
    }

    #[test]
    fn is_tool_loop_cancelled_recognizes_stream_cancelled_after_output() {
        let e = anyhow::Error::new(StreamCancelledAfterOutput::new("txt".to_string()));
        assert!(is_tool_loop_cancelled(&e));
    }
}
