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

/// A transport stream failure before user-visible output. The cumulative usage
/// snapshot is retained so Reliable recovery can bill the exact selected
/// stream attempt once.
#[derive(Debug)]
pub(crate) struct StreamErrorWithUsage {
    pub(crate) message: String,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
}

impl std::fmt::Display for StreamErrorWithUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamErrorWithUsage {}

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

/// Whether a turn error has the canonical semantic-empty terminal reason.
pub fn is_semantic_empty_terminal_completion(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source.is::<zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion>()
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

/// Map typed terminal-delivery failures to their Fluent user-facing message.
pub fn terminal_completion_error_message(
    err: &anyhow::Error,
    agent_name: Option<&str>,
) -> Option<String> {
    if is_semantic_empty_terminal_completion(err) {
        return Some(semantic_empty_terminal_completion_message(agent_name));
    }
    err.chain()
        .any(|source| source.is::<StreamPreExecutedToolsWithoutFinalResponse>())
        .then(|| pre_executed_tools_without_final_response_message(agent_name))
}

#[derive(Debug)]
pub(crate) struct StreamCancelledAfterOutput {
    pub(crate) partial_text: String,
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
    cause: ToolLoopCancelled,
}

impl StreamCancelledAfterOutput {
    pub(crate) fn with_usage(
        partial_text: String,
        usage: Option<zeroclaw_providers::traits::TokenUsage>,
    ) -> Self {
        Self {
            partial_text,
            usage,
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

/// Cancellation before caller-visible output. The cause remains the canonical
/// tool-loop cancellation while usage survives for rejected accounting.
#[derive(Debug)]
pub(crate) struct StreamCancelledWithUsage {
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
    cause: ToolLoopCancelled,
}

impl StreamCancelledWithUsage {
    pub(crate) fn new(usage: Option<zeroclaw_providers::traits::TokenUsage>) -> Self {
        Self {
            usage,
            cause: ToolLoopCancelled,
        }
    }
}

impl std::fmt::Display for StreamCancelledWithUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for StreamCancelledWithUsage {
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
        let direct = zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion;
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
        let error =
            anyhow::Error::new(zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion);
        assert!(is_semantic_empty_terminal_completion(&error));
        assert_eq!(
            terminal_completion_error_message(&error, None),
            Some("The model provider returned an invalid semantic completion.".to_string())
        );
    }

    #[test]
    fn disk_catalog_override_changes_delivery_not_the_diagnostic() {
        let error =
            anyhow::Error::new(zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion);
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
        let e = StreamCancelledAfterOutput::with_usage("partial text".to_string(), None);
        assert_eq!(e.to_string(), "tool loop cancelled after streamed output");
        assert_eq!(e.partial_text, "partial text");
    }

    #[test]
    fn stream_cancelled_after_output_source_chains_to_tool_loop_cancelled() {
        use std::error::Error;
        let e = StreamCancelledAfterOutput::with_usage(String::new(), None);
        let source = e.source().expect("must have source");
        assert!(source.is::<ToolLoopCancelled>());
    }

    #[test]
    fn is_tool_loop_cancelled_recognizes_stream_cancelled_after_output() {
        let e = anyhow::Error::new(StreamCancelledAfterOutput::with_usage(
            "txt".to_string(),
            None,
        ));
        assert!(is_tool_loop_cancelled(&e));
    }
}
