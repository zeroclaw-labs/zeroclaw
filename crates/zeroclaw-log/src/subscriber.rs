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
    /// and forwards only its metadata, so this asserts both halves of that
    /// bargain: no fragment of any dependency message text reaches either
    /// sink — not the credentials, not the names and secrets a heuristic
    /// would have waved through, not even the prose around them — while the
    /// severity, the dependency's own target, its module path and its source
    /// `file:line` all arrive intact, which is what makes the surviving
    /// record worth persisting.
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
        // transport failure is now findable only by its target, which is the
        // point — target plus `file:line` names the dependency call site
        // precisely enough to read the wording from its own source.
        let failure = bridged
            .iter()
            .find(|record| record["attributes"]["log.target"] == "whatsapp_rust::socket")
            .unwrap_or_else(|| {
                panic!(
                    "the dependency's transport failure must still be persisted, \
                     addressable by its target: {persisted}"
                )
            });
        assert_eq!(
            failure["severity_text"], "WARN",
            "the failure must keep its severity: {failure}"
        );
        assert_eq!(
            failure["attributes"]["log.target"], "whatsapp_rust::socket",
            "the failure must keep the dependency's own target so RUST_LOG \
             directives still address it: {failure}"
        );
        assert!(
            failure["attributes"]["log.module_path"]
                .as_str()
                .is_some_and(|path| path.starts_with("zeroclaw_log")),
            "the failure must keep its module path: {failure}"
        );
        assert!(
            failure["attributes"]["log.file"]
                .as_str()
                .is_some_and(|file| file.ends_with("subscriber.rs")),
            "the failure must keep its source file: {failure}"
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
                target: "zeroclaw_log_bridge_probe",
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
            line.contains("zeroclaw_log_bridge_probe"),
            "bridged record must keep the dependency's own target so RUST_LOG \
             directives still address it: {line:?}"
        );
        assert!(
            !line.contains("log.target"),
            "normalized metadata must replace the raw `log.*` transport fields \
             rather than printing them alongside: {line:?}"
        );
    }
}
