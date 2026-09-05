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
    cause: StreamInterruptionCause,
}

/// The reason an already-visible provider stream cannot complete. A typed
/// terminal cause must survive this boundary: delivery code distinguishes a
/// provider-declared incomplete response from an ordinary transport break.
#[derive(Debug)]
enum StreamInterruptionCause {
    Transport {
        message: String,
        usage: Option<zeroclaw_providers::traits::TokenUsage>,
    },
    Terminal(zeroclaw_api::model_provider::TerminalCompletionFailure),
    SemanticEmpty(zeroclaw_api::model_provider::SemanticEmptyTerminalFailure),
    ReliableProvider {
        message: String,
        usage: Option<zeroclaw_providers::traits::TokenUsage>,
        failure: zeroclaw_providers::ReliableProviderTerminalFailure,
    },
}

impl StreamInterruptedAfterOutput {
    pub(crate) fn transport(
        partial_text: String,
        message: String,
        usage: Option<zeroclaw_providers::traits::TokenUsage>,
    ) -> Self {
        Self {
            partial_text,
            cause: StreamInterruptionCause::Transport { message, usage },
        }
    }

    pub(crate) fn terminal(
        partial_text: String,
        failure: zeroclaw_api::model_provider::TerminalCompletionFailure,
    ) -> Self {
        Self {
            partial_text,
            cause: StreamInterruptionCause::Terminal(failure),
        }
    }

    /// Preserve a semantic-empty terminal cause when text was already shown.
    ///
    /// This is deliberately distinct from a transport interruption: the
    /// immutable prefix prevents replay, while the error chain still tells
    /// delivery and accounting that the provider completed without a final
    /// response after provider-side work.
    pub(crate) fn semantic_empty(
        partial_text: String,
        failure: zeroclaw_api::model_provider::SemanticEmptyTerminalFailure,
    ) -> Self {
        Self {
            partial_text,
            cause: StreamInterruptionCause::SemanticEmpty(failure),
        }
    }

    /// Preserve an already-classified Reliable cause after visible output.
    /// The partial response still prevents replay.
    pub(crate) fn reliable_provider(
        partial_text: String,
        message: String,
        usage: Option<zeroclaw_providers::traits::TokenUsage>,
        failure: zeroclaw_providers::ReliableProviderTerminalFailure,
    ) -> Self {
        Self {
            partial_text,
            cause: StreamInterruptionCause::ReliableProvider {
                message,
                usage,
                failure,
            },
        }
    }

    pub(crate) fn usage(&self) -> Option<&zeroclaw_providers::traits::TokenUsage> {
        match &self.cause {
            StreamInterruptionCause::Transport { usage, .. } => usage.as_ref(),
            StreamInterruptionCause::Terminal(failure) => failure.usage.as_ref(),
            StreamInterruptionCause::SemanticEmpty(failure) => failure.usage.as_ref(),
            StreamInterruptionCause::ReliableProvider { usage, .. } => usage.as_ref(),
        }
    }
}

impl std::fmt::Display for StreamInterruptedAfterOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cause {
            StreamInterruptionCause::Transport { message, .. } => f.write_str(message),
            StreamInterruptionCause::Terminal(failure) => failure.fmt(f),
            StreamInterruptionCause::SemanticEmpty(failure) => failure.fmt(f),
            StreamInterruptionCause::ReliableProvider { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for StreamInterruptedAfterOutput {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.cause {
            StreamInterruptionCause::Transport { .. } => None,
            StreamInterruptionCause::Terminal(failure) => Some(failure),
            StreamInterruptionCause::SemanticEmpty(failure) => Some(failure),
            StreamInterruptionCause::ReliableProvider { failure, .. } => Some(failure),
        }
    }
}

/// A no-output stream reached a provider-declared terminal incomplete state.
/// The active Reliable accounting scope owns the selected stream identity, so
/// this outcome carries only the provider failure and its recovery policy.
#[derive(Debug)]
pub(crate) struct StreamTerminalCompletion {
    pub(crate) failure: zeroclaw_api::model_provider::TerminalCompletionFailure,
    pub(crate) policy: zeroclaw_providers::TerminalCompletionPolicy,
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
    /// Preserve a classified terminal cause through the no-replay boundary.
    /// Provider-executed work still owns the recovery decision, but delivery
    /// must not collapse an output limit or invalid terminal state into a
    /// generic provider-tools message.
    pub(crate) cause: Option<StreamPreExecutedToolsCause>,
}

#[derive(Debug)]
pub(crate) enum StreamPreExecutedToolsCause {
    Terminal(zeroclaw_api::model_provider::TerminalCompletionFailure),
    Reliable(zeroclaw_providers::ReliableProviderTerminalFailure),
}

impl std::fmt::Display for StreamPreExecutedToolsWithoutFinalResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider stream ended after provider-executed tools without a final response")
    }
}

impl std::error::Error for StreamPreExecutedToolsWithoutFinalResponse {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self.cause.as_ref()? {
            StreamPreExecutedToolsCause::Terminal(cause) => Some(cause),
            StreamPreExecutedToolsCause::Reliable(cause) => Some(cause),
        }
    }
}

/// A completed stream that contains neither final text nor native tool calls.
/// Its reported usage survives the error boundary so retries and fallback do
/// not hide already-billed provider work from turn cost accounting.
#[derive(Debug)]
pub(crate) struct StreamSemanticEmptyCompletion {
    pub(crate) usage: Option<zeroclaw_providers::traits::TokenUsage>,
    pub(crate) replayable: bool,
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
            || source.is::<zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion>()
            || source.is::<StreamSemanticEmptyCompletion>()
            || source.is::<zeroclaw_providers::ReliableSemanticEmptyCompletion>()
    })
}

/// Render the canonical user-facing failure at a delivery boundary.
pub fn semantic_empty_terminal_completion_message(agent_name: Option<&str>) -> String {
    semantic_empty_terminal_completion_message_with_renderer(agent_name, render_cli_string)
}

type CliStringRenderer = fn(&str, &[(&str, &str)]) -> String;

fn render_cli_string(key: &str, args: &[(&str, &str)]) -> String {
    if args.is_empty() {
        crate::i18n::get_required_cli_string(key)
    } else {
        crate::i18n::get_required_cli_string_with_args(key, args)
    }
}

fn semantic_empty_terminal_completion_message_with_renderer(
    agent_name: Option<&str>,
    render: CliStringRenderer,
) -> String {
    match agent_name {
        Some(agent_name) => render(
            "cli-delegate-error-invalid-semantic-completion",
            &[("agent_name", agent_name)],
        ),
        None => render("cli-agent-error-invalid-semantic-completion", &[]),
    }
}

fn pre_executed_tools_without_final_response_message(
    agent_name: Option<&str>,
    render: CliStringRenderer,
) -> String {
    match agent_name {
        Some(agent_name) => render(
            "cli-delegate-error-incomplete-after-provider-tools",
            &[("agent_name", agent_name)],
        ),
        None => render("cli-agent-error-incomplete-after-provider-tools", &[]),
    }
}

fn reliable_provider_terminal_failure_message_with_renderer(
    failure: &zeroclaw_providers::ReliableProviderTerminalFailure,
    render: CliStringRenderer,
) -> String {
    use zeroclaw_providers::ReliableProviderTerminalFailureKind;

    match failure.kind() {
        ReliableProviderTerminalFailureKind::ContextWindow => {
            render("cli-agent-error-provider-context-window", &[])
        }
        ReliableProviderTerminalFailureKind::CredentialsMissing => match failure.provider() {
            Some(provider) => render(
                "cli-agent-error-provider-credentials-missing-named",
                &[("provider", provider)],
            ),
            None => render("cli-agent-error-provider-credentials-missing", &[]),
        },
        ReliableProviderTerminalFailureKind::Authentication => match failure.provider() {
            Some(provider) => render(
                "cli-agent-error-provider-authentication-named",
                &[("provider", provider)],
            ),
            None => render("cli-agent-error-provider-authentication", &[]),
        },
        ReliableProviderTerminalFailureKind::RateLimited => {
            render("cli-agent-error-provider-rate-limited", &[])
        }
        ReliableProviderTerminalFailureKind::ProviderServer => {
            render("cli-agent-error-provider-server", &[])
        }
        ReliableProviderTerminalFailureKind::ModelNotFound => {
            render("cli-agent-error-provider-model-not-found", &[])
        }
        ReliableProviderTerminalFailureKind::ClientRequest => {
            render("cli-agent-error-provider-client-request", &[])
        }
        ReliableProviderTerminalFailureKind::Connection => match failure.endpoint() {
            Some(endpoint) if failure.endpoint_is_local() => render(
                "cli-agent-error-provider-connection-local",
                &[("endpoint", endpoint)],
            ),
            Some(endpoint) => render(
                "cli-agent-error-provider-connection-remote",
                &[("endpoint", endpoint)],
            ),
            None => render("cli-agent-error-provider-connection", &[]),
        },
        ReliableProviderTerminalFailureKind::Timeout => {
            render("cli-agent-error-provider-timeout", &[])
        }
        ReliableProviderTerminalFailureKind::Other => {
            render("cli-agent-error-provider-generic", &[])
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
    terminal_completion_error_message_with_renderer(err, agent_name, render_cli_string)
}

fn terminal_completion_error_message_with_renderer(
    err: &anyhow::Error,
    agent_name: Option<&str>,
    render: CliStringRenderer,
) -> Option<String> {
    if zeroclaw_api::model_provider::semantic_empty_terminal_failure(err)
        .is_some_and(|failure| failure.has_pre_executed_tool_activity())
    {
        return Some(pre_executed_tools_without_final_response_message(
            agent_name, render,
        ));
    }
    if is_semantic_empty_terminal_completion(err) {
        return Some(semantic_empty_terminal_completion_message_with_renderer(
            agent_name, render,
        ));
    }
    if let Some(reason) = zeroclaw_api::model_provider::terminal_completion_error(err) {
        return Some(terminal_reason_message(reason, agent_name));
    }
    if let Some(failure) = err.chain().find_map(|source| {
        source.downcast_ref::<zeroclaw_providers::ReliableProviderTerminalFailure>()
    }) {
        return Some(reliable_provider_terminal_failure_message_with_renderer(
            failure, render,
        ));
    }
    err.chain()
        .any(|source| source.is::<StreamPreExecutedToolsWithoutFinalResponse>())
        .then(|| pre_executed_tools_without_final_response_message(agent_name, render))
}

#[cfg(test)]
fn terminal_completion_error_message_in_english(
    err: &anyhow::Error,
    agent_name: Option<&str>,
) -> Option<String> {
    terminal_completion_error_message_with_renderer(
        err,
        agent_name,
        crate::i18n::get_english_cli_string_with_args,
    )
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

/// Cancellation before caller-visible output. The canonical cancellation cause
/// remains intact while usage stays available for rejected-attempt accounting.
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
        let direct = SemanticEmptyTerminalCompletion;
        let stream = StreamSemanticEmptyCompletion {
            usage: None,
            replayable: true,
        };
        let provider_tools = StreamPreExecutedToolsWithoutFinalResponse {
            usage: None,
            cause: None,
        };

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
            terminal_completion_error_message_in_english(&error, None),
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
    fn reliable_provider_cause_uses_localized_delivery_without_retry_envelope() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let diagnostic = "All model providers/models failed after 3 failure event(s). Events: \
                          event 1 (retry 1/3): retryable";
        let error = anyhow::Error::new(ReliableProviderTerminalFailure::new(
            ReliableProviderTerminalFailureKind::Connection,
            Some("http://127.0.0.1:11434/v1/chat/completions".to_string()),
            diagnostic.to_string(),
        ));

        assert_eq!(error.to_string(), diagnostic);
        assert_eq!(
            terminal_completion_error_message_in_english(&error, None),
            Some(
                "The local model server at http://127.0.0.1:11434/v1/chat/completions is \
                 unavailable. Start it or update the endpoint."
                    .to_string()
            )
        );
        assert!(
            !terminal_completion_error_message_in_english(&error, None)
                .expect("typed provider failure must project")
                .contains("All model providers/models failed")
        );
    }

    #[test]
    fn reliable_provider_cause_distinguishes_remote_endpoint_guidance() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let error = anyhow::Error::new(ReliableProviderTerminalFailure::new(
            ReliableProviderTerminalFailureKind::Connection,
            Some("https://api.example.com/v1/chat/completions".to_string()),
            "All model providers/models failed after 1 failure event(s).".to_string(),
        ));

        assert_eq!(
            terminal_completion_error_message_in_english(&error, None),
            Some(
                "Cannot reach the model provider at https://api.example.com/v1/chat/completions. \
                 Check network access or choose another provider."
                    .to_string()
            )
        );
    }

    #[test]
    fn reliable_context_window_cause_uses_concise_delivery_message() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let error = anyhow::Error::new(ReliableProviderTerminalFailure::new(
            ReliableProviderTerminalFailureKind::ContextWindow,
            None,
            "Request exceeds model context window. Failed after 1 failure event(s).".to_string(),
        ));

        assert_eq!(
            terminal_completion_error_message_in_english(&error, None),
            Some(
                "The request is too large for the selected model. Reduce the conversation or choose \
                 a model with a larger context window."
                    .to_string()
            )
        );
    }

    #[test]
    fn reliable_provider_failure_kinds_use_their_fluent_messages() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let cases = [
            (
                ReliableProviderTerminalFailureKind::ContextWindow,
                "cli-agent-error-provider-context-window",
            ),
            (
                ReliableProviderTerminalFailureKind::CredentialsMissing,
                "cli-agent-error-provider-credentials-missing",
            ),
            (
                ReliableProviderTerminalFailureKind::Authentication,
                "cli-agent-error-provider-authentication",
            ),
            (
                ReliableProviderTerminalFailureKind::RateLimited,
                "cli-agent-error-provider-rate-limited",
            ),
            (
                ReliableProviderTerminalFailureKind::ProviderServer,
                "cli-agent-error-provider-server",
            ),
            (
                ReliableProviderTerminalFailureKind::ModelNotFound,
                "cli-agent-error-provider-model-not-found",
            ),
            (
                ReliableProviderTerminalFailureKind::ClientRequest,
                "cli-agent-error-provider-client-request",
            ),
            (
                ReliableProviderTerminalFailureKind::Connection,
                "cli-agent-error-provider-connection",
            ),
            (
                ReliableProviderTerminalFailureKind::Timeout,
                "cli-agent-error-provider-timeout",
            ),
            (
                ReliableProviderTerminalFailureKind::Other,
                "cli-agent-error-provider-generic",
            ),
        ];

        for (kind, key) in cases {
            let error = anyhow::Error::new(ReliableProviderTerminalFailure::new(
                kind,
                None,
                "All model providers/models failed after 1 failure event(s).".to_string(),
            ));

            assert_eq!(
                terminal_completion_error_message_in_english(&error, None),
                Some(crate::i18n::get_english_cli_string_with_args(key, &[])),
                "{kind:?} must use its dedicated Fluent message"
            );
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
    fn reliable_provider_credentials_messages_include_configured_provider() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let missing = anyhow::Error::new(
            ReliableProviderTerminalFailure::new(
                ReliableProviderTerminalFailureKind::CredentialsMissing,
                None,
                "full retry diagnostic".to_string(),
            )
            .with_provider("custom.truefoundry"),
        );
        let rejected = anyhow::Error::new(
            ReliableProviderTerminalFailure::new(
                ReliableProviderTerminalFailureKind::Authentication,
                None,
                "full retry diagnostic".to_string(),
            )
            .with_provider("custom.truefoundry"),
        );

        assert_eq!(
            terminal_completion_error_message_in_english(&missing, None),
            Some(
                "The model provider custom.truefoundry has no configured credentials. Add its API key or choose another provider."
                    .to_string()
            )
        );
        assert_eq!(
            terminal_completion_error_message_in_english(&rejected, None),
            Some(
                "The model provider custom.truefoundry rejected its credentials. Check the configured credentials."
                    .to_string()
            )
        );
    }

    #[test]
    fn visible_stream_terminal_failure_keeps_its_typed_delivery_cause() {
        use zeroclaw_api::model_provider::{TerminalCompletionError, TerminalCompletionFailure};
        use zeroclaw_providers::traits::TokenUsage;

        let error = anyhow::Error::new(StreamInterruptedAfterOutput::terminal(
            "partial".to_string(),
            TerminalCompletionFailure::new(
                TerminalCompletionError::OutputTokenLimit,
                Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(4),
                    cached_input_tokens: None,
                }),
            ),
        ));

        assert_eq!(
            terminal_completion_error_message(&error, None),
            Some(
                "The provider reached its output token limit before completing the response."
                    .to_string()
            )
        );
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<TerminalCompletionFailure>()
                .is_some_and(|failure| failure.reason == TerminalCompletionError::OutputTokenLimit)
        }));
    }

    #[test]
    fn provider_tool_no_replay_outcome_keeps_typed_terminal_delivery_cause() {
        use zeroclaw_api::model_provider::{TerminalCompletionError, TerminalCompletionFailure};

        let error = anyhow::Error::new(StreamPreExecutedToolsWithoutFinalResponse {
            usage: None,
            cause: Some(StreamPreExecutedToolsCause::Terminal(
                TerminalCompletionFailure::from(TerminalCompletionError::OutputTokenLimit),
            )),
        });

        assert_eq!(
            terminal_completion_error_message(&error, None),
            Some(
                "The provider reached its output token limit before completing the response."
                    .to_string()
            )
        );
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<TerminalCompletionFailure>()
                .is_some_and(|failure| failure.reason == TerminalCompletionError::OutputTokenLimit)
        }));
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
