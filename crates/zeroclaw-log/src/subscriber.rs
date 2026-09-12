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
    // Fail loudly: the subscriber above is already installed, so a discarded
    // error here would leave the daemon looking healthy while every
    // dependency record stays missing.
    crate::log_bridge::install_or_panic();
}

/// Test-only subscriber install. Best-effort by design: a test binary calls
/// this once per test, and both the `tracing` global default and the `log`
/// logger slot accept exactly one installation per process, so every call
/// after the first necessarily fails and is deliberately ignored. Production
/// daemons go through [`install_global_subscriber`], which panics instead.
///
/// Not part of the public API despite the `pub`: it is reachable only so
/// other workspace crates' `#[cfg(test)]` modules can install the pipeline.
#[doc(hidden)]
pub fn try_install_capture_subscriber() {
    use tracing_subscriber::Registry;
    let subscriber = Registry::default().with(LogCaptureLayer);
    let _ = tracing::subscriber::set_global_default(subscriber);
    crate::log_bridge::install_best_effort_for_tests();
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

    /// Message shapes lifted from `whatsapp-rust` at the revision this
    /// workspace pins (`cbcdd2a`): `src/pair_code.rs` logs the configured
    /// phone number and the generated pair code at `INFO`, `src/message.rs`
    /// logs JIDs, and the transport logs a failure with no identifier in it
    /// at all.
    const PAIR_PHONE: &str = "972501234567";
    const PAIR_CODE: &str = "3K7XW2QZ";
    /// A pair code that drew no digits out of the Crockford alphabet — about
    /// one code in twenty, and the shape a digits-only rule would leak.
    const PAIR_CODE_ALL_LETTERS: &str = "ZXKWQTRV";
    const PEER_JID: &str = "972501234567@s.whatsapp.net";
    const TRANSPORT_FAILURE: &str = "websocket read failed: connection reset by peer";

    /// Free-text shapes that no message heuristic can classify: an ordinary
    /// given name, a mixed-case brand, a non-ASCII name, and a secret with no
    /// digit and no `@` in it. Each is indistinguishable from prose by
    /// inspection, which is why the boundary does not inspect.
    const PERSON_NAME: &str = "Alice";
    const BRAND_NAME: &str = "Acme";
    const UNICODE_NAME: &str = "Zoë Müller";
    const NONNUMERIC_SECRET: &str = "sk_live_zzyxwvutsrq";

    /// Every third-party `log` record emitted by the body below. Pins that
    /// the bridge forwards each one as a record rather than dropping it.
    const BRIDGED_RECORD_COUNT: usize = 8;

    /// The credential boundary, exercised through the real sinks rather than
    /// the fmt probe: the global `LogCaptureLayer`, `writer::record_event`'s
    /// rolling JSONL persistence, and the broadcast hook, wired exactly as
    /// the daemon wires them.
    ///
    /// A bare `LogTracer` would hand every third-party `log` message to those
    /// sinks as ordinary event text, so `whatsapp-rust`'s `INFO` pair-code and
    /// phone-number lines would land in `runtime-trace.jsonl` and on the live
    /// stream — bypassing the `LoginEvent::PairCode` -> `ephemeral_attrs`
    /// boundary and `record_event`'s guarantee that pairing credentials are
    /// never persisted.
    ///
    /// The bridge instead drops the message body of every third-party record
    /// and forwards only bounded metadata, so this asserts both halves of
    /// that bargain: no fragment of any dependency message text reaches
    /// either sink — not the credentials, not the names and secrets a
    /// heuristic would have waved through, not even the prose around them —
    /// while the severity, the dependency's own target and its source line
    /// all arrive intact, which is what makes the surviving record worth
    /// persisting. The unbounded `module_path` and `file` channels are gone;
    /// the `log_bridge` module docs give the reasoning.
    #[test]
    fn dependency_records_reach_the_sinks_without_their_message_text() {
        let _writer_guard = crate::writer::WRITER_TEST_LOCK.lock();
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();

        let tmp = tempfile::tempdir().unwrap();
        let cfg = crate::config::LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: 1000,
            ..crate::config::LogConfig::default()
        };
        crate::writer::init_from_config(&cfg, tmp.path());

        let (tx, mut rx) = tokio::sync::broadcast::channel(256);
        crate::broadcast::set_broadcast_hook(tx);

        // Real entry point: installs the capture layer *and* the bridge.
        crate::try_install_capture_subscriber();

        let subscriber = tracing_subscriber::registry().with(LogCaptureLayer);
        tracing::subscriber::with_default(subscriber, || {
            log::info!(
                target: "Client/PairCode",
                "Starting pair code authentication for phone: {PAIR_PHONE}"
            );
            log::info!(
                target: "Client/PairCode",
                "Stage 1 complete, waiting for phone confirmation. Code: {PAIR_CODE}"
            );
            log::info!(
                target: "Client/PairCode",
                "Stage 1 complete, waiting for phone confirmation. Code: {PAIR_CODE_ALL_LETTERS}"
            );
            log::warn!(
                target: "whatsapp_rust::message",
                "Failed to parse message info (from={PEER_JID}): bad MAC"
            );
            log::info!(
                target: "whatsapp_rust::message",
                "delivering receipt to contact {PERSON_NAME} of {BRAND_NAME}"
            );
            log::info!(
                target: "whatsapp_rust::message",
                "push name updated to {UNICODE_NAME}"
            );
            log::warn!(
                target: "whatsapp_rust::handshake",
                "rejected credential {NONNUMERIC_SECRET}"
            );
            log::warn!(
                target: "whatsapp_rust::socket",
                "{TRANSPORT_FAILURE}"
            );
        });

        let mut broadcast = Vec::new();
        while let Ok(value) = rx.try_recv() {
            broadcast.push(value.to_string());
        }
        crate::broadcast::clear_broadcast_hook();

        crate::writer::flush_for_test().unwrap();
        let persisted =
            std::fs::read_to_string(crate::writer::runtime_trace_path().unwrap()).unwrap();

        for (sink, body) in [
            ("persisted runtime-trace.jsonl", persisted.as_str()),
            ("live broadcast", broadcast.concat().as_str()),
        ] {
            for (what, marker) in [
                ("phone number", PAIR_PHONE),
                ("pair code", PAIR_CODE),
                ("all-letter pair code", PAIR_CODE_ALL_LETTERS),
                ("peer JID", PEER_JID),
                // The four shapes a message heuristic cannot rule on. Each
                // one is plain prose to any classifier, and each one used to
                // cross verbatim.
                ("person name", PERSON_NAME),
                ("brand name", BRAND_NAME),
                ("non-ASCII name", UNICODE_NAME),
                ("nonnumeric secret", NONNUMERIC_SECRET),
                // Not just the payloads: no third-party message text at all,
                // including the prose that framed them and a failure line
                // with no identifier in it.
                ("message prose", "Starting pair code authentication"),
                ("identifier-free failure text", TRANSPORT_FAILURE),
            ] {
                assert!(
                    !body.contains(marker),
                    "third-party {what} must not reach the {sink}: {body}"
                );
            }
        }

        // Every emission still arrives as a record: the text is withheld, the
        // event is not suppressed.
        let bridged: Vec<serde_json::Value> = persisted
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value["attributes"]["log.target"].is_string())
            .collect();
        assert_eq!(
            bridged.len(),
            BRIDGED_RECORD_COUNT,
            "every third-party record must still be persisted, message withheld \
             rather than event dropped: {persisted}"
        );
        for record in &bridged {
            assert_eq!(
                record["message"],
                crate::log_bridge::REDACTED_MESSAGE,
                "every bridged message body must be the fixed marker: {record}"
            );
        }
        assert_eq!(
            broadcast.len(),
            BRIDGED_RECORD_COUNT,
            "every third-party record must also reach the live broadcast: {broadcast:?}"
        );

        // What the bridge is still worth: severity and provenance. The
        // transport failure is findable by its crate's reviewed name — a
        // module-path target reduces to the crate, since everything after
        // `::` is a runtime string — plus its severity and line. The
        // hand-written literals (`Client/PairCode` above) keep their full
        // reviewed spelling.
        let failure = bridged
            .iter()
            .find(|record| {
                record["attributes"]["log.target"] == "whatsapp_rust"
                    && record["severity_text"] == "WARN"
                    && record["attributes"]["log.line"].as_u64().is_some()
            })
            .unwrap_or_else(|| {
                panic!(
                    "the dependency's transport failure must still be persisted, \
                     addressable by its crate name: {persisted}"
                )
            });
        assert_eq!(
            failure["attributes"]["log.target"], "whatsapp_rust",
            "the failure must carry the crate's reviewed name so RUST_LOG \
             directives still address it: {failure}"
        );
        assert!(
            failure["attributes"]["log.module_path"].is_null(),
            "the module path is an unbounded string channel and must not be \
             forwarded at all: {failure}"
        );
        assert!(
            failure["attributes"]["log.file"].is_null(),
            "the source file is an unbounded string channel and must not be \
             forwarded at all: {failure}"
        );
        assert!(
            failure["attributes"]["log.line"].as_u64().is_some(),
            "the failure must keep its source line: {failure}"
        );

        // Severities are per-record, not flattened onto the marker.
        assert_eq!(
            bridged
                .iter()
                .filter(|record| record["severity_text"] == "INFO")
                .count(),
            5,
            "each record must keep its own severity: {persisted}"
        );
    }

    /// Markers a dependency could put in a record's metadata at runtime.
    /// Deliberately identifier-shaped for the two dropped channels: if
    /// `module_path` and `file` were sanitized rather than dropped, these
    /// would sail through any charset rule.
    const DYNAMIC_TARGET_PAYLOAD: &str = "zeroclaw_dynamic_target_marker";
    const DYNAMIC_MODULE_PAYLOAD: &str = "zeroclaw_dynamic_module_marker";
    const DYNAMIC_FILE_PAYLOAD: &str = "zeroclaw_dynamic_file_marker";
    /// Runtime targets that FIT the retired charset rule — bare numeric,
    /// underscore-separated secret-shaped, identifier-shaped — and must be
    /// stopped anyway: fitness is provenance against the reviewed tables,
    /// not shape.
    const NUMERIC_TARGET_PAYLOAD: &str = "31337000073313370001";
    const SECRET_TARGET_PAYLOAD: &str = "sk_live_zeroclaw_marker_token";
    const NAME_TARGET_PAYLOAD: &str = "ZeroclawDynamicNameMarker";

    /// The metadata half of the credential boundary, through the same real
    /// sinks: the global `LogCaptureLayer`, the rolling JSONL writer and the
    /// broadcast hook.
    ///
    /// `log::RecordBuilder` takes a borrowed `&str` for `target`,
    /// `module_path` and `file`, and the `log!` macros take a `target:`
    /// expression rather than a literal, so a dependency can put a runtime
    /// value in any of the three. Forwarding them as trusted provenance
    /// therefore reopens the same persistence path the message-body
    /// redaction closes: `tracing_log` normalizes them into `log.target` /
    /// `log.module_path` / `log.file`, `LogCaptureLayer` puts unrecognized
    /// fields into the attributes map, and `writer::record_event` sends that
    /// map to both broadcast and disk.
    ///
    /// So: `module_path` and `file` are dropped outright and must not appear
    /// even when their content is a plain identifier, and a target outside
    /// the documented safe representation is replaced whole rather than
    /// trimmed to its acceptable characters.
    #[test]
    fn dynamic_record_metadata_never_reaches_the_sinks() {
        let _writer_guard = crate::writer::WRITER_TEST_LOCK.lock();
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();

        let tmp = tempfile::tempdir().unwrap();
        let cfg = crate::config::LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: 1000,
            ..crate::config::LogConfig::default()
        };
        crate::writer::init_from_config(&cfg, tmp.path());

        let (tx, mut rx) = tokio::sync::broadcast::channel(256);
        crate::broadcast::set_broadcast_hook(tx);

        // Real entry point: installs the capture layer *and* the bridge.
        crate::try_install_capture_subscriber();

        // Built by hand rather than through `log::warn!`, because the macro
        // fills `module_path` and `file` from `module_path!()` and
        // `Location::caller()`. This is the shape a dependency reaches for
        // when it builds a record itself — which the `log` API permits.
        let dynamic_target = format!("dependency for {DYNAMIC_TARGET_PAYLOAD}!");

        let subscriber = tracing_subscriber::registry().with(LogCaptureLayer);
        tracing::subscriber::with_default(subscriber, || {
            for target in [
                dynamic_target.as_str(),
                NUMERIC_TARGET_PAYLOAD,
                SECRET_TARGET_PAYLOAD,
                NAME_TARGET_PAYLOAD,
            ] {
                log::logger().log(
                    &log::Record::builder()
                        .args(format_args!("third-party wording"))
                        .level(log::Level::Warn)
                        .target(target)
                        .module_path(Some(DYNAMIC_MODULE_PAYLOAD))
                        .file(Some(DYNAMIC_FILE_PAYLOAD))
                        .line(Some(4242))
                        .build(),
                );
            }
        });

        let mut broadcast = Vec::new();
        while let Ok(value) = rx.try_recv() {
            broadcast.push(value.to_string());
        }
        crate::broadcast::clear_broadcast_hook();

        crate::writer::flush_for_test().unwrap();
        let persisted =
            std::fs::read_to_string(crate::writer::runtime_trace_path().unwrap()).unwrap();

        for (sink, body) in [
            ("persisted runtime-trace.jsonl", persisted.as_str()),
            ("live broadcast", broadcast.concat().as_str()),
        ] {
            for (what, marker) in [
                ("target", DYNAMIC_TARGET_PAYLOAD),
                ("module path", DYNAMIC_MODULE_PAYLOAD),
                ("source file", DYNAMIC_FILE_PAYLOAD),
                ("phone-shaped numeric target", NUMERIC_TARGET_PAYLOAD),
                ("secret-shaped target", SECRET_TARGET_PAYLOAD),
                ("name-shaped target", NAME_TARGET_PAYLOAD),
                ("message body", "third-party wording"),
            ] {
                assert!(
                    !body.contains(marker),
                    "a runtime value in a record's {what} must not reach the {sink}: {body}"
                );
            }
        }

        // Not silently dropped: the records still arrive, carrying the two
        // constants and nothing the caller chose but the severity and line.
        let bridged: Vec<serde_json::Value> = persisted
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value["attributes"]["log.target"].is_string())
            .collect();
        assert_eq!(
            bridged.len(),
            4,
            "a record with unsafe metadata must still be recorded, sanitized \
             rather than suppressed: {persisted}"
        );
        for record in &bridged {
            assert_eq!(
                record["attributes"]["log.target"],
                crate::log_bridge::REDACTED_TARGET,
                "an unsafe target must be replaced whole, not trimmed: {record}"
            );
            assert_eq!(
                record["message"],
                crate::log_bridge::REDACTED_MESSAGE,
                "the message body must still be the fixed marker: {record}"
            );
            assert!(
                record["attributes"]["log.module_path"].is_null()
                    && record["attributes"]["log.file"].is_null(),
                "the dropped channels must be absent, not sanitized: {record}"
            );
            assert_eq!(
                record["attributes"]["log.line"], 4242,
                "the numeric line still crosses: {record}"
            );
        }

        // That the sanitizer is not simply eating every target is pinned by
        // `dependency_records_reach_the_sinks_without_their_message_text`,
        // which sees `Client/PairCode` cross verbatim and
        // `whatsapp_rust::socket` cross as its crate's reviewed name through
        // this same wiring.
    }

    /// The other half of keeping `RUST_LOG` working: a bridged record must be
    /// selected by an `EnvFilter` directive naming the dependency's own
    /// target, and a record from another target must not be.
    #[test]
    fn env_filter_directives_still_select_bridged_records_by_target() {
        crate::try_install_capture_subscriber();

        let buf = BufMakeWriter::default();
        let fmt_layer = fmt::layer()
            .fmt_fields(RedactEphemeralFields)
            .with_writer(buf.clone())
            .with_ansi(false)
            .event_format(AgentAliasFormatter::new())
            .with_filter(EnvFilter::new("whatsapp_rust=debug"));
        let subscriber = tracing_subscriber::registry().with(fmt_layer);

        tracing::subscriber::with_default(subscriber, || {
            log::debug!(target: "whatsapp_rust::socket", "selected");
            log::debug!(target: "some_other_dependency", "not selected");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            out.contains("whatsapp_rust"),
            "`RUST_LOG=whatsapp_rust=debug` must still select the dependency's \
             records by its crate name: {out:?}"
        );
        assert!(
            !out.contains("whatsapp_rust::socket"),
            "the module path after `::` is a runtime string and must not \
             survive into the emitted target: {out:?}"
        );
        assert!(
            !out.contains("some_other_dependency"),
            "a target the directive does not name must stay filtered out: {out:?}"
        );
    }

    /// One default-target call site per activated crate, spelled as the
    /// module path `log` derives from `module_path!()` when the call passes
    /// no `target:`. Each is a real site at pin `cbcdd2a`; the expected root
    /// is what `TARGET_CRATES` must reduce it to.
    const ACTIVATED_CRATE_SITES: &[(&str, &str)] = &[
        ("whatsapp_rust::message", "whatsapp_rust"),
        ("wacore::send", "wacore"),
        (
            "wacore_libsignal::protocol::session_cipher",
            "wacore_libsignal",
        ),
        ("wacore_noise::framing", "wacore_noise"),
        (
            "whatsapp_rust_tokio_transport",
            "whatsapp_rust_tokio_transport",
        ),
    ];

    /// Whether the fmt layer rendered a record under exactly this target.
    /// `tracing_log` puts the bridged target back on the normalized metadata,
    /// so the fmt line reads `DEBUG <target>: <message>`. Matched as a whole
    /// token because a `contains` would let the `wacore` directive look
    /// satisfied by a `wacore_libsignal` record, which is the distinction
    /// these tests exist to make.
    fn renders_target(out: &str, target: &str) -> bool {
        let rendered = format!("{target}:");
        out.split_whitespace().any(|token| token == rendered)
    }

    /// Every crate the `whatsapp-web` feature activates that logs without an
    /// explicit `target:` must arrive at the real sinks under its own
    /// reviewed root, not under the shared redaction marker.
    ///
    /// `module_path` and `file` are dropped at the boundary, so the target is
    /// the only field left that says which component spoke. A crate missing
    /// from `TARGET_CRATES` still fails closed — its record crosses as
    /// `[third-party target redacted]` — but that makes an activated part of
    /// the WhatsApp protocol stack indistinguishable from an unreviewed
    /// transitive dependency, which is the opposite of what this bridge is
    /// for. So the coverage is pinned against the locked graph rather than
    /// left to the two crates that happened to be read first.
    #[test]
    fn every_activated_crate_reaches_the_sinks_under_its_own_root() {
        let _writer_guard = crate::writer::WRITER_TEST_LOCK.lock();
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();

        let tmp = tempfile::tempdir().unwrap();
        let cfg = crate::config::LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: 1000,
            ..crate::config::LogConfig::default()
        };
        crate::writer::init_from_config(&cfg, tmp.path());

        let (tx, mut rx) = tokio::sync::broadcast::channel(256);
        crate::broadcast::set_broadcast_hook(tx);

        crate::try_install_capture_subscriber();

        let subscriber = tracing_subscriber::registry().with(LogCaptureLayer);
        tracing::subscriber::with_default(subscriber, || {
            for (site, _) in ACTIVATED_CRATE_SITES {
                log::warn!(target: *site, "{TRANSPORT_FAILURE}");
            }
        });

        let mut broadcast = Vec::new();
        while let Ok(value) = rx.try_recv() {
            broadcast.push(value.to_string());
        }
        crate::broadcast::clear_broadcast_hook();

        crate::writer::flush_for_test().unwrap();
        let persisted =
            std::fs::read_to_string(crate::writer::runtime_trace_path().unwrap()).unwrap();

        let bridged: Vec<serde_json::Value> = persisted
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value["attributes"]["log.target"].is_string())
            .collect();
        assert_eq!(
            bridged.len(),
            ACTIVATED_CRATE_SITES.len(),
            "every activated crate's record must be persisted: {persisted}"
        );
        assert_eq!(
            broadcast.len(),
            ACTIVATED_CRATE_SITES.len(),
            "every activated crate's record must also be broadcast: {broadcast:?}"
        );

        for ((site, root), record) in ACTIVATED_CRATE_SITES.iter().zip(&bridged) {
            assert_eq!(
                record["attributes"]["log.target"], *root,
                "`{site}` must arrive under its crate's reviewed root, not the \
                 redaction marker: {record}"
            );
            assert_eq!(
                record["message"],
                crate::log_bridge::REDACTED_MESSAGE,
                "the message body is still withheld for every crate: {record}"
            );
            assert!(
                record["attributes"]["log.module_path"].is_null()
                    && record["attributes"]["log.file"].is_null(),
                "the dropped channels stay dropped for every crate: {record}"
            );
            assert_eq!(
                record["severity_text"], "WARN",
                "severity survives for every crate: {record}"
            );
        }

        // The other half of the decision: the packages in the same activated
        // graph that emit nothing on the default target are absent from the
        // vocabulary, so a record claiming their root is redacted like any
        // other unreviewed one.
        assert_eq!(
            crate::log_bridge::TARGET_CRATES.len(),
            ACTIVATED_CRATE_SITES.len(),
            "the vocabulary and the reviewed call-site list must describe the \
             same set of crates"
        );
    }

    /// The filter contract per crate: a directive naming a crate selects that
    /// crate's bridged records, and `EnvFilter` matches a directive target as
    /// a prefix, so the family directives documented on `TARGET_CRATES`
    /// (`wacore=debug`, `whatsapp_rust=debug`) reach the sibling crates whose
    /// roots extend them. Pinned per directive rather than described, because
    /// the reduction happens before the filters look and a missing root would
    /// silently stop being addressable.
    #[test]
    fn each_activated_crate_is_selectable_by_its_own_directive() {
        crate::try_install_capture_subscriber();

        for (directive, expected) in [
            (
                "wacore",
                &["wacore", "wacore_libsignal", "wacore_noise"][..],
            ),
            ("wacore_libsignal", &["wacore_libsignal"][..]),
            ("wacore_noise", &["wacore_noise"][..]),
            (
                "whatsapp_rust",
                &["whatsapp_rust", "whatsapp_rust_tokio_transport"][..],
            ),
            (
                "whatsapp_rust_tokio_transport",
                &["whatsapp_rust_tokio_transport"][..],
            ),
        ] {
            let buf = BufMakeWriter::default();
            let fmt_layer = fmt::layer()
                .fmt_fields(RedactEphemeralFields)
                .with_writer(buf.clone())
                .with_ansi(false)
                .event_format(AgentAliasFormatter::new())
                .with_filter(EnvFilter::new(format!("{directive}=debug")));
            let subscriber = tracing_subscriber::registry().with(fmt_layer);

            tracing::subscriber::with_default(subscriber, || {
                for (site, _) in ACTIVATED_CRATE_SITES {
                    log::debug!(target: *site, "{TRANSPORT_FAILURE}");
                }
            });

            let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            for (_, root) in ACTIVATED_CRATE_SITES {
                assert_eq!(
                    renders_target(&out, root),
                    expected.contains(root),
                    "`RUST_LOG={directive}=debug` must {} `{root}`: {out:?}",
                    if expected.contains(root) {
                        "select"
                    } else {
                        "not select"
                    }
                );
            }
        }
    }

    /// `log_enabled!` must answer with the active tracing filter, the way
    /// `tracing_log::LogTracer` does. The bridge sets `log`'s own max level to
    /// `Trace`, so an unconditional `enabled` makes every guarded diagnostic
    /// in the dependency tree look wanted: the caller builds the record and
    /// evaluates its formatting arguments, and only then does the dispatcher
    /// throw it away.
    ///
    /// The subscriber here filters globally rather than per layer, because
    /// `Subscriber::enabled` is the only thing `log` can ask and per-layer
    /// filters deliberately do not answer through it — see the note on
    /// `RedactingLogBridge::enabled`.
    #[test]
    fn log_enabled_agrees_with_the_active_tracing_filter() {
        crate::try_install_capture_subscriber();

        let off = tracing_subscriber::registry()
            .with(LogCaptureLayer)
            .with(EnvFilter::new("off"));
        tracing::subscriber::with_default(off, || {
            assert!(
                !log::log_enabled!(target: "unmatched_dependency_target", log::Level::Info),
                "`log_enabled!` must be false under an `off` filter"
            );
        });

        let selective = tracing_subscriber::registry()
            .with(LogCaptureLayer)
            .with(EnvFilter::new("whatsapp_rust=debug"));
        tracing::subscriber::with_default(selective, || {
            assert!(
                log::log_enabled!(target: "whatsapp_rust", log::Level::Debug),
                "`log_enabled!` must be true for a target the filter selects"
            );
            assert!(
                !log::log_enabled!(target: "unmatched_dependency_target", log::Level::Debug),
                "`log_enabled!` must be false for a target the filter excludes"
            );
        });
    }

    /// The production wiring is different, and the difference is intentional:
    /// `install_global_subscriber` attaches its `EnvFilter`s *per layer*, and
    /// a per-layer filter deliberately answers `Subscriber::enabled` with
    /// `true`, deferring the real decision to `on_event` so the other layers
    /// get their say. So under the production shape, `log_enabled!` is gated
    /// by the process-wide max level but NOT by target-specific directives —
    /// a dependency's guarded diagnostic may be built and then dropped at the
    /// layer. Documented on `RedactingLogBridge::enabled`; this pins the
    /// behavior with the production filter shape so the limitation stays a
    /// described one rather than a silent one.
    #[test]
    fn log_enabled_under_per_layer_filters_is_level_gated_only() {
        crate::try_install_capture_subscriber();

        let buf = BufMakeWriter::default();
        let fmt_layer = fmt::layer()
            .fmt_fields(RedactEphemeralFields)
            .with_writer(buf.clone())
            .with_ansi(false)
            .event_format(AgentAliasFormatter::new())
            .with_filter(EnvFilter::new("whatsapp_rust=debug"));
        let subscriber = tracing_subscriber::registry().with(fmt_layer);

        tracing::subscriber::with_default(subscriber, || {
            // Only the target half is pinnable here: the other gate,
            // `LevelFilter::current()`, is a process-global hint this scoped
            // subscriber cannot isolate from the test binary's global one.
            // The target half answers `true` by design: `Filtered::enabled`
            // defers, and the layer drops the event later.
            assert!(
                log::log_enabled!(target: "unmatched_dependency_target", log::Level::Debug),
                "a per-layer filter cannot make `log_enabled!` target-aware; \
                 if this starts failing, the limitation documented on \
                 `RedactingLogBridge::enabled` has changed"
            );
            // And the drop happens where it belongs — the record built after
            // that `true` still never reaches the layer's output.
            log::debug!(target: "unmatched_dependency_target", "guarded diagnostic");
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            out.is_empty(),
            "the unmatched record must be dropped by the per-layer filter \
             even though `log_enabled!` said yes: {out:?}"
        );
    }

    /// Blocker-2 contract: the production install path is loud when the
    /// process-global `log` slot is already owned, because a discarded error
    /// there leaves the tracing subscriber installed and the dependency
    /// records permanently missing. The test-only path stays tolerant so a
    /// shared test binary can call it once per test.
    #[test]
    fn log_bridge_install_is_loud_in_production_and_tolerant_in_tests() {
        // Guarantee the slot is occupied regardless of test ordering: either
        // this call wins it, or an earlier test already did.
        crate::log_bridge::install_best_effort_for_tests();

        let panicked = std::panic::catch_unwind(crate::log_bridge::install_or_panic);
        let payload = panicked.expect_err(
            "the production install path must not silently accept a conflicting logger",
        );
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("another logger already owns the process-global `log` slot"),
            "the panic must name the conflict so an operator can act on it: {message:?}"
        );

        // The test-only path swallows the same failure by design.
        crate::log_bridge::install_best_effort_for_tests();
    }

    /// Regression for dependency logs vanishing without a trace: transports
    /// like `whatsapp-rust` emit through the `log` facade, not `tracing`.
    /// Installing a `tracing` subscriber leaves `log`'s own global logger
    /// slot empty, and every such record used to be discarded at the macro's
    /// max-level check — reaching neither stderr nor the JSONL trace at any
    /// verbosity. Installing this crate's subscriber machinery must be
    /// enough on its own for a bare `log::warn!` to arrive as a tracing
    /// event; removing the bridge install makes this test go silent.
    ///
    /// It arrives with its text withheld: stderr is a sink like any other, so
    /// the bridged line shows the redaction marker under the dependency's own
    /// target and severity, never the dependency's wording.
    #[test]
    fn log_facade_records_reach_the_subscriber() {
        // Real entry point: installs the capture layer *and* the bridge.
        crate::try_install_capture_subscriber();

        let buf = BufMakeWriter::default();
        let fmt_layer = fmt::layer()
            .fmt_fields(RedactEphemeralFields)
            .with_writer(buf.clone())
            .with_ansi(false)
            .event_format(AgentAliasFormatter::new());
        let subscriber = tracing_subscriber::registry().with(fmt_layer);

        tracing::subscriber::with_default(subscriber, || {
            log::warn!(
                target: "whatsapp_rust::bridge_probe",
                "dependency log facade marker"
            );
        });

        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            !out.contains("dependency log facade marker"),
            "the dependency's own message text must not reach stderr either: {out:?}"
        );
        let line = out
            .lines()
            .find(|line| line.contains(crate::log_bridge::REDACTED_MESSAGE))
            .unwrap_or_else(|| {
                panic!(
                    "a `log` record must reach the tracing subscriber; without the \
                     bridge it is dropped at the log facade: {out:?}"
                )
            });
        assert!(
            line.starts_with("[system] "),
            "bridged record must go through the alias-prefixing formatter: {line:?}"
        );
        assert!(
            line.contains("WARN"),
            "bridged record must keep its severity: {line:?}"
        );
        assert!(
            line.contains("whatsapp_rust"),
            "bridged record must carry the crate's reviewed name so RUST_LOG \
             directives still address it: {line:?}"
        );
        assert!(
            !line.contains("bridge_probe"),
            "the module path after `::` is a runtime string and must not \
             survive into the emitted target: {line:?}"
        );
        assert!(
            !line.contains("log.target"),
            "normalized metadata must replace the raw `log.*` transport fields \
             rather than printing them alongside: {line:?}"
        );
    }
}
