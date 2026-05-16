//! Global tracing-subscriber installation. The only public entry
//! point a daemon binary needs. Owns the agent-alias-prefixed
//! formatter and the `LogCaptureLayer` wiring so the rest of the
//! workspace never names a `tracing` or `tracing_subscriber` type.

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

use crate::event::ZeroclawAttribution;
use crate::layer::LogCaptureLayer;

/// Install the global tracing subscriber: stderr fmt with the
/// agent-alias-prefixed formatter on top + the `LogCaptureLayer` that
/// routes structured events to the JSONL writer, broadcast hook, and
/// Observer bridge.
///
/// `default_filter` is the `RUST_LOG`-compatible filter string used
/// when the environment variable is unset (e.g. `"info"` or
/// `"info,matrix_sdk=warn"`).
///
/// Panics on subscriber install failure — the daemon cannot operate
/// without logging.
pub fn install_global_subscriber(default_filter: &str) {
    let subscriber = fmt::Subscriber::builder()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .event_format(AgentAliasFormatter::new())
        .finish()
        .with(LogCaptureLayer);

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");
}

/// Test-only helper: install a minimal global subscriber that routes
/// `record!` emissions through `LogCaptureLayer` (and thus the broadcast
/// hook) without any terminal fmt output. Returns a guard that resets
/// the broadcast hook on drop. Use in combination with
/// [`crate::subscribe`] to capture events from a unit test without
/// the test crate depending on `tracing` / `tracing-subscriber`.
///
/// Idempotent: subsequent calls are no-ops if a subscriber is already
/// installed (the global default cannot be replaced once set). For
/// isolated capture across multiple tests, use the broadcast hook
/// directly without changing the global subscriber.
#[doc(hidden)]
pub fn try_install_capture_subscriber() {
    use tracing_subscriber::Registry;
    let subscriber = Registry::default().with(LogCaptureLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Tracing event formatter that prefixes each log line with the most
/// specific alias-bound label available in the current span scope.
/// `agent_alias` wins; falls back to the channel composite; finally
/// to `[system]` for boot / migration / install-wide messages.
struct AgentAliasFormatter {
    inner: fmt::format::Format<fmt::format::Full, fmt::time::SystemTime>,
}

impl AgentAliasFormatter {
    fn new() -> Self {
        Self {
            inner: fmt::format::Format::default(),
        }
    }
}

impl<S, N> fmt::FormatEvent<S, N> for AgentAliasFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &fmt::FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let label = ctx
            .event_scope()
            .and_then(|scope| {
                scope.into_iter().find_map(|span| {
                    span.extensions()
                        .get::<ZeroclawAttribution>()
                        .and_then(|attribution| {
                            attribution
                                .get("agent_alias")
                                .or_else(|| attribution.get("channel"))
                                .map(str::to_string)
                        })
                })
            })
            .unwrap_or_else(|| "system".to_string());
        write!(writer, "[{label}] ")?;
        self.inner.format_event(ctx, writer, event)
    }
}
