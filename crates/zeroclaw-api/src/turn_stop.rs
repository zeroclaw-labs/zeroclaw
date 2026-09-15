//! Typed stop taxonomy for the agent turn path.
//!
//! Every control-flow abort on the turn path carries a [`TurnStop`] so
//! callers classify by downcast instead of matching on message text. The
//! `Display` of a `TurnStop` is exactly its `detail`, so an error tagged this
//! way stringifies identically to the `anyhow::bail!` it replaced and the
//! existing string heuristics keep working as the fallback for errors that
//! originate outside our code (raw transport errors, provider bodies).

/// How the turn should be treated once it stopped.
///
/// Declarative metadata: nothing branches on it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStopClass {
    /// The condition may clear on its own; retrying the turn is reasonable.
    Recoverable,
    /// The turn is over but its work so far is worth reporting to the user.
    CloseOut,
    /// The turn cannot proceed and retrying it will not help.
    Fatal,
}

/// What stopped the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStopCode {
    /// The pattern-based loop detector tripped its circuit breaker.
    LoopDetector,
    /// Consecutive rounds produced byte-identical tool output.
    IdenticalOutput,
    /// The tool loop exhausted `max_tool_iterations`.
    MaxIterations,
    /// The turn's cost budget is spent.
    BudgetExhausted,
    /// The request does not fit the model's context window and history
    /// cannot be trimmed further.
    ContextOverflow,
    /// The provider rejected the credentials.
    ProviderAuth,
    /// No provider/model in the chain could serve the request.
    ProviderUnavailable,
    /// A single inference step exceeded `pacing.step_timeout_secs`.
    StepTimeout,
    /// The whole turn exceeded its time budget.
    TurnTimeout,
    /// The caller cancelled the turn.
    Cancelled,
    /// A task driving the turn panicked.
    Panicked,
    /// A prompt-required tool was called again with identical arguments
    /// before the pending approval resolved.
    PromptRequiredRepeat,
}

/// A typed control-flow abort on the turn path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStop {
    /// How the turn should be treated.
    pub class: TurnStopClass,
    /// What stopped it.
    pub code: TurnStopCode,
    /// The message the turn would have carried anyway; this is the whole of
    /// the `Display`, so tagging never changes what an error stringifies to.
    pub detail: String,
}

impl TurnStop {
    /// Build a stop with the given classification and message.
    pub fn new(class: TurnStopClass, code: TurnStopCode, detail: impl Into<String>) -> Self {
        Self {
            class,
            code,
            detail: detail.into(),
        }
    }

    /// A stop whose condition may clear on its own.
    pub fn recoverable(code: TurnStopCode, detail: impl Into<String>) -> Self {
        Self::new(TurnStopClass::Recoverable, code, detail)
    }

    /// A stop that ends the turn but leaves its work worth reporting.
    pub fn close_out(code: TurnStopCode, detail: impl Into<String>) -> Self {
        Self::new(TurnStopClass::CloseOut, code, detail)
    }

    /// A stop that cannot be retried.
    pub fn fatal(code: TurnStopCode, detail: impl Into<String>) -> Self {
        Self::new(TurnStopClass::Fatal, code, detail)
    }
}

impl std::fmt::Display for TurnStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for TurnStop {}

/// An error that already existed, carrying a [`TurnStop`] classification.
///
/// `Display` and the source chain are the cause's, so tagging an error this
/// way is invisible to every consumer that does not ask for the stop.
#[derive(Debug)]
struct Tagged {
    stop: TurnStop,
    cause: anyhow::Error,
}

impl std::fmt::Display for Tagged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.cause, f)
    }
}

impl std::error::Error for Tagged {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// Classify an error that came from somewhere else, keeping what it says.
pub fn tag(err: anyhow::Error, stop: TurnStop) -> anyhow::Error {
    anyhow::Error::new(Tagged { stop, cause: err })
}

/// Find the [`TurnStop`] an error carries, anywhere in its source chain.
///
/// Mirrors `is_tool_loop_cancelled`: walk `chain()` so a stop survives being
/// wrapped in `anyhow` context or attached to a foreign error by [`tag`].
pub fn turn_stop(err: &anyhow::Error) -> Option<&TurnStop> {
    err.chain().find_map(|source| {
        source
            .downcast_ref::<TurnStop>()
            .or_else(|| source.downcast_ref::<Tagged>().map(|t| &t.stop))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_exactly_the_detail() {
        let stop = TurnStop::close_out(TurnStopCode::MaxIterations, "Agent exceeded maximum");
        assert_eq!(stop.to_string(), "Agent exceeded maximum");
        assert_eq!(
            anyhow::Error::new(stop).to_string(),
            "Agent exceeded maximum"
        );
    }

    #[test]
    fn turn_stop_finds_a_direct_stop() {
        let err = anyhow::Error::new(TurnStop::fatal(TurnStopCode::BudgetExhausted, "spent"));
        let found = turn_stop(&err).expect("stop must be found");
        assert_eq!(found.code, TurnStopCode::BudgetExhausted);
        assert_eq!(found.class, TurnStopClass::Fatal);
    }

    #[test]
    fn turn_stop_finds_a_context_wrapped_stop() {
        let err = anyhow::Error::new(TurnStop::close_out(TurnStopCode::StepTimeout, "slow"))
            .context("while calling the provider");
        assert_eq!(
            turn_stop(&err).expect("stop must survive context").code,
            TurnStopCode::StepTimeout
        );
    }

    #[test]
    fn turn_stop_is_none_for_a_plain_error() {
        assert!(turn_stop(&anyhow::Error::msg("something else")).is_none());
    }

    #[test]
    fn tag_keeps_what_the_error_says() {
        let tagged = tag(
            anyhow::Error::msg("prompt is too long"),
            TurnStop::fatal(TurnStopCode::ContextOverflow, "context overflow"),
        );
        assert_eq!(tagged.to_string(), "prompt is too long");
        assert_eq!(
            turn_stop(&tagged).expect("tagged stop").code,
            TurnStopCode::ContextOverflow
        );
    }
}
