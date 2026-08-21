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
//! So the bridge is a scrubbing adapter, not a pass-through. A target
//! allowlist alone cannot draw the line — in `whatsapp-rust` the useful
//! failure diagnostics and the identifier-bearing lines share a target — so
//! the boundary sits on the message text and is **deny by default**: a token
//! is forwarded verbatim only when it cannot express an identifier or a
//! credential, and anything else becomes [`REDACTED`]. Severity, target,
//! module path and source location are structured metadata, never free text,
//! and are forwarded untouched so `RUST_LOG` directives and the failure's
//! provenance still work.
//!
//! Note the deliberate contrast with [`zeroclaw_memory::redact`], which is
//! allow-by-default: it rewrites *recognized* patterns in user content the
//! operator opted into storing. Here the input is unreviewed third-party text
//! entering a credential-adjacent sink, so an unclassifiable token must be
//! dropped rather than passed.
//!
//! [`zeroclaw_memory::redact`]: https://docs.rs/zeroclaw-memory

use tracing_log::AsTrace;

/// Placeholder substituted for any token the scrubber cannot vouch for.
pub(crate) const REDACTED: &str = "[redacted]";

/// Longest bare-decimal token forwarded verbatim. Sized below every
/// identifier shape this boundary defends against: an E.164 phone number
/// carries at least 7 digits and a `whatsapp-rust` pair code is 8 Crockford
/// Base32 characters, so four digits cannot spell either. Keeps close codes
/// (`1006`), HTTP statuses and small counts readable in a failure line.
const MAX_BARE_DIGITS: usize = 4;

/// Shortest all-uppercase-letter run treated as a possible pair code. The
/// generator emits 8 Crockford Base32 characters, and roughly one code in
/// twenty comes out with no digit at all, so an all-caps run cannot be waved
/// through on "it has no digits". Mixed-case words (`WebSocket`) and short
/// all-caps words (`WARN`, `HTTP`, `TLS`, `IQ`) stay readable.
const MIN_UPPERCASE_RUN: usize = 6;

/// Scrub a third-party log message, deny by default.
///
/// Whitespace and token order are preserved so the line still reads; each
/// whitespace-delimited token is either forwarded verbatim or replaced whole
/// by [`REDACTED`], with any surrounding ASCII punctuation kept so the
/// sentence does not lose its shape.
///
/// A token survives only when, ignoring leading/trailing ASCII punctuation,
/// it satisfies all of:
///
/// * it contains no `@` — that is a JID or an email address;
/// * it contains no numeric character, *unless* it is at most
///   [`MAX_BARE_DIGITS`] ASCII digits and nothing else;
/// * it is not a run of [`MIN_UPPERCASE_RUN`] or more ASCII uppercase
///   letters.
///
/// Everything else — phone numbers, pair codes, JIDs, LIDs, session and
/// message ids, keys, hex and Base64 blobs — falls to the default and is
/// redacted, because the boundary cannot tell them apart from the identifiers
/// it exists to withhold.
pub(crate) fn scrub(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while !rest.is_empty() {
        let token_start = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..token_start]);
        rest = &rest[token_start..];
        if rest.is_empty() {
            break;
        }
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        push_scrubbed_token(&mut out, &rest[..token_end]);
        rest = &rest[token_end..];
    }
    out
}

fn push_scrubbed_token(out: &mut String, token: &str) {
    let core = token.trim_matches(|c: char| c.is_ascii_punctuation());
    if core.is_empty() || core_is_safe(core) {
        out.push_str(token);
        return;
    }
    // Keep the punctuation that framed the token (`(`, `,`, `:`, `.`) so the
    // surrounding sentence still parses; only the payload is replaced.
    let lead = token.len()
        - token
            .trim_start_matches(|c: char| c.is_ascii_punctuation())
            .len();
    let trail = token.len() - core.len() - lead;
    out.push_str(&token[..lead]);
    out.push_str(REDACTED);
    out.push_str(&token[token.len() - trail..]);
}

fn core_is_safe(core: &str) -> bool {
    if core.contains('@') {
        return false;
    }
    if core.chars().any(char::is_numeric) {
        return core.len() <= MAX_BARE_DIGITS && core.bytes().all(|b| b.is_ascii_digit());
    }
    !(core.len() >= MIN_UPPERCASE_RUN && core.chars().all(|c| c.is_ascii_uppercase()))
}

/// The `log` logger installed in the process-global slot. Forwards each
/// record into `tracing` through [`tracing_log::format_trace`], which is the
/// same dispatch [`tracing_log::LogTracer`] performs — identical callsite,
/// identical `log.target` / `log.module_path` / `log.file` / `log.line`
/// normalization — except that the message it carries has been through
/// [`scrub`].
struct ScrubbingLogBridge;

static BRIDGE: ScrubbingLogBridge = ScrubbingLogBridge;

impl log::Log for ScrubbingLogBridge {
    /// Always true: `log`-side filtering would silently pre-empt the
    /// `RUST_LOG` directives, so the tracing filters stay the single place
    /// that decides what is recorded.
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        // Ask the subscriber first so a record no layer wants costs no
        // rendering or scrubbing. `format_trace` repeats this check; doing it
        // here keeps the chatty `DEBUG`/`TRACE` tiers of a dependency off the
        // allocator when the filter floor is `INFO`.
        if !tracing::dispatcher::get_default(|dispatch| dispatch.enabled(&record.as_trace())) {
            return;
        }
        let scrubbed = scrub(&record.args().to_string());
        // Rebuilt field by field rather than copied from the incoming record:
        // this is the whole list of things allowed across the boundary, so a
        // future `log` release that grows another payload channel (structured
        // key-values, say) cannot ride along unreviewed.
        let _ = tracing_log::format_trace(
            &log::Record::builder()
                .args(format_args!("{scrubbed}"))
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

/// Install the scrubbing bridge into the process-global `log` slot.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_redacts_pairing_credentials_and_identifiers() {
        // Message shapes from whatsapp-rust `src/pair_code.rs` at the locked
        // revision, plus a JID-bearing line from `src/message.rs`.
        assert_eq!(
            scrub("Starting pair code authentication for phone: 972501234567"),
            format!("Starting pair code authentication for phone: {REDACTED}")
        );
        assert_eq!(
            scrub("Stage 1 complete, waiting for phone confirmation. Code: 3K7XW2QZ"),
            format!("Stage 1 complete, waiting for phone confirmation. Code: {REDACTED}")
        );
        // A pair code that happens to draw no digits from the Crockford
        // alphabet must not slip through the digit rule.
        assert_eq!(scrub("Code: ZXKWQTRV"), format!("Code: {REDACTED}"));
        assert_eq!(
            scrub("Failed to decrypt from 972501234567@s.whatsapp.net"),
            format!("Failed to decrypt from {REDACTED}")
        );
        assert_eq!(
            scrub("group 120363012345678901@g.us went silent"),
            format!("group {REDACTED} went silent")
        );
        assert_eq!(
            scrub("phone: +972-50-123-4567"),
            format!("phone: +{REDACTED}")
        );
    }

    #[test]
    fn scrub_keeps_dependency_failure_diagnostics_readable() {
        for line in [
            "websocket read failed: connection reset by peer",
            "Failed to send companion_finish: broken pipe",
            "Missing or invalid primary identity pub in notification",
        ] {
            assert_eq!(scrub(line), line, "failure diagnostic must survive intact");
        }
        // Short bare numbers stay: close codes, statuses, counts.
        assert_eq!(
            scrub("socket closed with code 1006 after 3 retries"),
            "socket closed with code 1006 after 3 retries"
        );
        // Mixed case and short all-caps words are prose, not codes.
        assert_eq!(
            scrub("WebSocket handshake failed over TLS"),
            "WebSocket handshake failed over TLS"
        );
    }

    #[test]
    fn scrub_is_deny_by_default_for_unclassifiable_tokens() {
        // Neither a phone nor a JID nor a pair code, but equally
        // unvouchable: ids, keys, blobs, and long digit runs.
        for token in [
            "3EB0C767D26A1D8B1F49",
            "session-9f2c41ab",
            "1234567",
            "sk_live_abc123def456",
            "١٢٣٤٥٦٧٨",
        ] {
            let out = scrub(&format!("saw {token} here"));
            assert_eq!(
                out,
                format!("saw {REDACTED} here"),
                "unclassifiable token must fail closed: {token}"
            );
        }
    }

    #[test]
    fn scrub_preserves_whitespace_and_framing_punctuation() {
        assert_eq!(
            scrub("info (from=972501234567@s.whatsapp.net):\n\tdecrypt failed"),
            format!("info ({REDACTED}):\n\tdecrypt failed")
        );
        assert_eq!(scrub(""), "");
        assert_eq!(scrub("   "), "   ");
        assert_eq!(scrub("-->"), "-->");
    }
}
