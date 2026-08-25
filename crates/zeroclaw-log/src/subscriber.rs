//! Global tracing-subscriber installation. The only public entry
//! point a daemon binary needs. Owns the agent-alias-prefixed
//! formatter and the `LogCaptureLayer` wiring so the rest of the
//! workspace never names a `tracing` or `tracing_subscriber` type.

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::field::{RecordFields, VisitOutput};
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::format::{DefaultVisitor, Writer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

use crate::event::ZeroclawAttribution;
use crate::layer::{F_EPHEMERAL_ATTRS, LogCaptureLayer};

pub fn install_global_subscriber(
    recording_override: Option<&str>,
    default_filter: &str,
    verbose: bool,
) {
    // Recording floor: explicit flag wins, then RUST_LOG, then default.
    let recording_filter = match recording_override {
        Some(flag) => EnvFilter::new(flag),
        None => {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter))
        }
    };

    let fmt_filter = if verbose {
        match recording_override {
            Some(flag) => EnvFilter::new(flag),
            None => {
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter))
            }
        }
    } else {
        EnvFilter::new("off")
    };

    let fmt_layer = fmt::layer()
        .fmt_fields(RedactEphemeralFields)
        .with_writer(std::io::stderr)
        .event_format(AgentAliasFormatter::new())
        .with_filter(fmt_filter);

    let subscriber = tracing_subscriber::registry()
        .with(LogCaptureLayer.with_filter(recording_filter))
        .with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
}

#[doc(hidden)]
pub fn try_install_capture_subscriber() {
    use tracing_subscriber::Registry;
    let subscriber = Registry::default().with(LogCaptureLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Test support: install a process-global subscriber that hands every
/// formatted event line to `sink`, exactly where the stderr fmt layer would
/// write. Lets an integration test in another crate observe, and deliberately
/// stall, the terminal write path without naming `tracing` types outside this
/// crate. The sink runs unlocked on whichever thread emits the event, so a
/// sink that blocks on one event models a wedged consumer of that event
/// without serializing every other thread's unrelated writes behind it.
/// Mirrors the production wiring by stacking [`LogCaptureLayer`] under the
/// terminal layer, so `attribution_span!` scopes carry their attribution
/// extensions and both the structured and terminal outputs behave as in a
/// real daemon. Returns `false` when a global subscriber was already
/// installed.
#[doc(hidden)]
pub fn try_install_line_sink_for_tests(sink: impl Fn(&str) + Send + Sync + 'static) -> bool {
    use std::sync::Arc;
    use tracing_subscriber::fmt::MakeWriter;

    type SharedSink = Arc<dyn Fn(&str) + Send + Sync>;

    #[derive(Clone)]
    struct SinkMakeWriter(SharedSink);
    struct SinkGuard(SharedSink);

    impl std::io::Write for SinkGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let line = String::from_utf8_lossy(buf);
            (self.0)(&line);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SinkMakeWriter {
        type Writer = SinkGuard;
        fn make_writer(&'a self) -> Self::Writer {
            SinkGuard(self.0.clone())
        }
    }

    let shared: SharedSink = Arc::new(sink);
    // INFO floor: keeps third-party TRACE/DEBUG chatter (e.g. wasmtime
    // internals) from serializing through the sink while still delivering
    // every `record!`-level event a test would assert on.
    let fmt_layer = fmt::layer()
        .fmt_fields(RedactEphemeralFields)
        .with_writer(SinkMakeWriter(shared))
        .with_ansi(false)
        .event_format(AgentAliasFormatter::new())
        .with_filter(EnvFilter::new("info"));
    let subscriber = tracing_subscriber::registry()
        .with(LogCaptureLayer)
        .with(fmt_layer);
    tracing::subscriber::set_global_default(subscriber).is_ok()
}

/// Field formatter that renders event fields exactly like the default
/// formatter but drops the `zc_ephemeral_attrs` transport field, so
/// short-lived pairing credentials (QR payloads, pair codes) never reach the
/// terminal in verbose mode. The field still rides the event to the
/// `LogCaptureLayer`, which routes it onto the broadcast-only ephemeral path;
/// only the human-readable stderr display is redacted. All other fields keep
/// the default rendering (the delegated `DefaultVisitor` handles `message`
/// escaping, error sources, etc.).
struct RedactEphemeralFields;

impl<'writer> FormatFields<'writer> for RedactEphemeralFields {
    fn format_fields<R: RecordFields>(
        &self,
        writer: Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = RedactEphemeralVisitor {
            inner: DefaultVisitor::new(writer, true),
        };
        fields.record(&mut visitor);
        visitor.inner.finish()
    }
}

/// Visitor wrapper that forwards every field to the default visitor except
/// the ephemeral-attributes transport field, which it swallows. The current
/// transport records the credential via `%Display` (`record_debug`), but the
/// guard is applied uniformly across *every* visitor method so the redaction
/// stays fail-closed if the field's recorded representation ever changes
/// (numeric, boolean, error, str) — a leak must not reappear just because the
/// call site swapped `%` for `?` or a typed value.
struct RedactEphemeralVisitor<'a> {
    inner: DefaultVisitor<'a>,
}

impl RedactEphemeralVisitor<'_> {
    /// True when the field is the ephemeral-attributes transport field and
    /// must therefore be dropped from the human-readable stderr rendering.
    fn is_ephemeral(field: &Field) -> bool {
        field.name() == F_EPHEMERAL_ATTRS
    }
}

impl Visit for RedactEphemeralVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_debug(field, value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_str(field, value);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_error(field, value);
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_f64(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_i64(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_u64(field, value);
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_i128(field, value);
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_u128(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if Self::is_ephemeral(field) {
            return;
        }
        self.inner.record_bool(field, value);
    }
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
        let label = ctx.event_scope().and_then(|scope| {
            let mut channel = None;
            for span in scope {
                let extensions = span.extensions();
                let Some(attribution) = extensions.get::<ZeroclawAttribution>() else {
                    continue;
                };
                if let Some(agent_alias) = attribution.get("agent_alias") {
                    return Some(agent_alias.to_string());
                }
                if channel.is_none() {
                    channel = attribution.get("channel").map(str::to_string);
                }
            }
            channel
        });
        let label = label.as_deref().unwrap_or("system");
        write!(writer, "[{label}] ")?;
        self.inner.format_event(ctx, writer, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    /// In-memory `MakeWriter` so a test can capture what the fmt layer would
    /// have written to stderr in verbose mode.
    #[derive(Clone, Default)]
    struct BufMakeWriter(Arc<Mutex<Vec<u8>>>);

    struct BufGuard(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for BufGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufMakeWriter {
        type Writer = BufGuard;
        fn make_writer(&'a self) -> Self::Writer {
            BufGuard(self.0.clone())
        }
    }

    #[test]
    fn event_semantics_foreground_label_prefers_agent_across_nested_spans() {
        let buf = BufMakeWriter::default();
        let fmt_layer = fmt::layer()
            .fmt_fields(RedactEphemeralFields)
            .with_writer(buf.clone())
            .with_ansi(false)
            .event_format(AgentAliasFormatter::new());
        let subscriber = tracing_subscriber::registry()
            .with(LogCaptureLayer)
            .with(fmt_layer);

        tracing::subscriber::with_default(subscriber, || {
            {
                let outer_agent = tracing::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "outer_agent",
                    agent_alias = "outer"
                );
                let _outer_agent_guard = outer_agent.enter();
                let nearest_agent = tracing::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "nearest_agent",
                    agent_alias = "clamps"
                );
                let _nearest_agent_guard = nearest_agent.enter();
                let inner_channel = tracing::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "inner_channel",
                    channel = "discord.glados"
                );
                let _inner_channel_guard = inner_channel.enter();
                tracing::info!(
                    target: "zeroclaw_log_internal_test",
                    "agent precedence event"
                );
            }

            let outer_channel = tracing::info_span!(
                target: "zeroclaw_log_internal_scope",
                "outer_channel",
                channel = "telegram.outer"
            );
            let _outer_channel_guard = outer_channel.enter();
            let inner_channel = tracing::info_span!(
                target: "zeroclaw_log_internal_scope",
                "nearest_channel",
                channel = "discord.inner"
            );
            let _inner_channel_guard = inner_channel.enter();
            tracing::info!(
                target: "zeroclaw_log_internal_test",
                "channel precedence event"
            );
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let line = out
            .lines()
            .find(|line| line.contains("agent precedence event"))
            .expect("agent precedence event should be formatted");
        assert!(
            line.starts_with("[clamps] "),
            "nearest agent alias must take global precedence over a closer channel: {line:?}"
        );
        let line = out
            .lines()
            .find(|line| line.contains("channel precedence event"))
            .expect("channel precedence event should be formatted");
        assert!(
            line.starts_with("[discord.inner] "),
            "nearest channel must win when no agent alias is present: {line:?}"
        );
    }

    /// Regression for the ephemeral-credential-at-verbose-stderr leak: the
    /// terminal fmt layer must render the login event but never print the
    /// `zc_ephemeral_attrs` transport field (which carries the QR payload /
    /// pair code), so a supervisor or log collector scraping stderr in
    /// verbose mode cannot retain the pairing secret.
    #[test]
    fn verbose_terminal_output_redacts_ephemeral_credentials() {
        let buf = BufMakeWriter::default();
        let fmt_layer = fmt::layer()
            .fmt_fields(RedactEphemeralFields)
            .with_writer(buf.clone())
            .with_ansi(false)
            .event_format(AgentAliasFormatter::new());
        let subscriber = tracing_subscriber::registry().with(fmt_layer);

        tracing::subscriber::with_default(subscriber, || {
            crate::record!(
                INFO,
                crate::Event::new(module_path!(), crate::Action::Note)
                    .with_attrs(::serde_json::json!({"login": {"state": "qr"}}))
                    .with_ephemeral_attrs(::serde_json::json!({
                        "login": {"qr_payload": "SUPER-SECRET-QR-MARKER"}
                    })),
                "qr pairing login event"
            );
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("qr pairing login event"),
            "the login event should still be logged: {out:?}"
        );
        assert!(
            !out.contains("SUPER-SECRET-QR-MARKER"),
            "verbose stderr must not contain the ephemeral pairing credential: {out:?}"
        );
        assert!(
            !out.contains(F_EPHEMERAL_ATTRS),
            "the ephemeral transport field must be dropped entirely: {out:?}"
        );
    }

    /// Fail-closed across representations: even if the ephemeral transport
    /// field were ever recorded as a typed value (str / integer / bool) rather
    /// than the current `%Display` debug form, the redactor must still drop it.
    /// Guards against a future call-site change silently reintroducing the
    /// leak through an unguarded visitor method.
    #[test]
    fn verbose_terminal_output_redacts_ephemeral_across_representations() {
        fn render_with<F: FnOnce()>(emit: F) -> String {
            let buf = BufMakeWriter::default();
            let fmt_layer = fmt::layer()
                .fmt_fields(RedactEphemeralFields)
                .with_writer(buf.clone())
                .with_ansi(false)
                .event_format(AgentAliasFormatter::new());
            let subscriber = tracing_subscriber::registry().with(fmt_layer);
            tracing::subscriber::with_default(subscriber, emit);
            String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
        }

        // str representation (record_str)
        let out = render_with(|| {
            tracing::info!(zc_ephemeral_attrs = "SECRET-STR-MARKER", "str ephemeral");
        });
        assert!(out.contains("str ephemeral"), "event still logged: {out:?}");
        assert!(
            !out.contains("SECRET-STR-MARKER"),
            "str-recorded ephemeral field leaked: {out:?}"
        );
        assert!(
            !out.contains(F_EPHEMERAL_ATTRS),
            "field name leaked: {out:?}"
        );

        // integer representation (record_i64)
        let out = render_with(|| {
            tracing::info!(zc_ephemeral_attrs = 424242i64, "int ephemeral");
        });
        assert!(out.contains("int ephemeral"), "event still logged: {out:?}");
        assert!(
            !out.contains("424242"),
            "integer-recorded ephemeral field leaked: {out:?}"
        );
        assert!(
            !out.contains(F_EPHEMERAL_ATTRS),
            "field name leaked: {out:?}"
        );

        // bool representation (record_bool)
        let out = render_with(|| {
            tracing::info!(zc_ephemeral_attrs = true, "bool ephemeral");
        });
        assert!(
            out.contains("bool ephemeral"),
            "event still logged: {out:?}"
        );
        assert!(
            !out.contains(F_EPHEMERAL_ATTRS),
            "bool-recorded ephemeral field leaked: {out:?}"
        );
    }
}
