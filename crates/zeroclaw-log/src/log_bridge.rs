//! Fail-closed bridge from the `log` facade into `tracing`.
//!
//! Dependencies log through `log`, not `tracing` — `whatsapp-rust` and
//! friends. Installing a `tracing` subscriber does nothing for them: `log`
//! keeps its own global logger slot, and while that slot is empty every
//! `log::warn!` in the dependency tree is discarded at the macro's own
//! max-level check. Those records reach neither stderr nor the JSONL trace,
//! so a transport failure inside a dependency leaves no evidence at any
//! verbosity.
//!
//! Filling that slot with a bare [`tracing_log::LogTracer`] would recover the
//! diagnostics *and* hand every third-party message string to
//! [`crate::layer::LogCaptureLayer`], which materializes it as ordinary event
//! text and persists it to `runtime-trace.jsonl` (rolling persistence is on by
//! default, at an `INFO` floor). Third-party `INFO`/`WARN` sites are not ours
//! to review: at the locked `whatsapp-rust` revision, `src/pair_code.rs` logs
//! the configured phone number and the generated pair code at `INFO`, and
//! other sites log JIDs. Those strings would bypass the deliberate
//! `LoginEvent::PairCode` → `ephemeral_attrs` boundary and
//! [`crate::writer::record_event`]'s guarantee that pairing credentials never
//! reach disk.
//!
//! So the bridge forwards a record's *metadata* and drops its *text*. The
//! message body is the one free-text channel a `log` record carries, it is
//! written by code this workspace does not review, and nothing here can tell
//! a harmless sentence from a name, a brand, an identifier or a credential.
//! So none of it crosses: every bridged record carries the fixed
//! [`REDACTED_MESSAGE`] marker in place of whatever the dependency formatted.
//! There is no heuristic and no allowlist, so there is no rule to get wrong
//! and nothing an unusual message shape can talk its way past.
//!
//! What does cross is structured metadata: severity, the dependency's own
//! target, its module path and its source `file:line`. Those are fixed at
//! each call site in the dependency's own source rather than formatted from
//! runtime values, so they carry no payload. They are also enough to keep the
//! diagnostics actionable — the severity says how bad it was, and the target
//! plus `file:line` name the exact line that fired, so the wording can be
//! read from the dependency's source instead of from our trace — and enough
//! for `RUST_LOG` directives to keep addressing a dependency by its own
//! target.
//!
//! Note the deliberate contrast with [`zeroclaw_memory::redact`], which is
//! allow-by-default: it rewrites *recognized* patterns in user content the
//! operator opted into storing. Here the input is unreviewed third-party text
//! entering a credential-adjacent sink, so none of it is passed.
//!
//! [`zeroclaw_memory::redact`]: https://docs.rs/zeroclaw-memory

use tracing_log::AsTrace;

/// Fixed text substituted for every third-party `log` message body.
///
/// Not a per-token placeholder: the message is never inspected, so this
/// replaces the whole of it every time. Its presence in a record is the
/// signal that a dependency logged at that call site and that the wording was
/// withheld by design rather than lost.
pub(crate) const REDACTED_MESSAGE: &str = "[third-party message redacted]";

/// The `log` logger installed in the process-global slot. Forwards each
/// record into `tracing` through [`tracing_log::format_trace`], which is the
/// same dispatch [`tracing_log::LogTracer`] performs — identical callsite,
/// identical `log.target` / `log.module_path` / `log.file` / `log.line`
/// normalization — except that the message it carries is always
/// [`REDACTED_MESSAGE`] instead of the dependency's own text.
struct RedactingLogBridge;

static BRIDGE: RedactingLogBridge = RedactingLogBridge;

impl log::Log for RedactingLogBridge {
    /// Always true: `log`-side filtering would silently pre-empt the
    /// `RUST_LOG` directives, so the tracing filters stay the single place
    /// that decides what is recorded.
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        // Ask the subscriber first so a record no layer wants costs nothing
        // to dispatch. `format_trace` repeats this check; doing it here keeps
        // a dependency's chatty `DEBUG`/`TRACE` tiers off the callsite
        // machinery when the filter floor is `INFO`.
        if !tracing::dispatcher::get_default(|dispatch| dispatch.enabled(&record.as_trace())) {
            return;
        }
        // Rebuilt field by field rather than copied from the incoming record:
        // this list is the whole of what may cross the boundary, so a future
        // `log` release that grows another payload channel (structured
        // key-values, say) cannot ride along unreviewed. `args` is the free
        // text channel and it is overwritten rather than forwarded — the
        // dependency's own message is never read.
        let _ = tracing_log::format_trace(
            &log::Record::builder()
                .args(format_args!("{REDACTED_MESSAGE}"))
                .level(record.level())
                .target(record.target())
                .module_path(record.module_path())
                .file(record.file())
                .line(record.line())
                .build(),
        );
    }

    fn flush(&self) {}
}

/// Install the redacting bridge into the process-global `log` slot.
///
/// Fails when another logger already owns that slot; `log` permits exactly
/// one per process.
fn install() -> Result<(), log::SetLoggerError> {
    log::set_logger(&BRIDGE)?;
    // The bridge decides nothing about verbosity, so let every record reach
    // it and let the tracing filters do the filtering.
    log::set_max_level(log::LevelFilter::Trace);
    Ok(())
}

/// Production install: panics when the bridge cannot take the `log` slot.
///
/// A silent failure here is worse than a crash. The tracing subscriber is
/// installed by the time this runs, so discarding the error would leave the
/// daemon looking healthy while the dependency records this bridge exists to
/// recover stay missing — the exact invisible-failure mode the bridge was
/// added to end.
pub(crate) fn install_or_panic() {
    if let Err(err) = install() {
        panic!(
            "installing the `log` -> tracing bridge failed ({err}): another logger already \
             owns the process-global `log` slot, so dependency diagnostics would be lost \
             silently. Remove the competing `log::set_logger` call."
        );
    }
}

/// Test-only install: tolerates the slot already being taken.
///
/// Test binaries call [`crate::try_install_capture_subscriber`] once per test,
/// and `log` allows a single logger per process, so every call after the first
/// necessarily fails. The already-installed logger is this same bridge, so
/// ignoring the error is correct *here and only here*. Production goes through
/// [`install_or_panic`].
pub(crate) fn install_best_effort_for_tests() {
    let _ = install();
}
