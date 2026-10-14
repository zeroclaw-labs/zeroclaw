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
//! diagnostics *and* hand every third-party string on the record to
//! [`crate::layer::LogCaptureLayer`], which materializes them as event text
//! and attributes and persists them to `runtime-trace.jsonl` (rolling
//! persistence is on by default, at an `INFO` floor). Third-party call sites
//! are not ours to review: at the locked `whatsapp-rust` revision,
//! `src/pair_code.rs` logs the configured phone number and the generated pair
//! code at `INFO`, and other sites log JIDs. Those strings would bypass the
//! deliberate `LoginEvent::PairCode` → `ephemeral_attrs` boundary and
//! [`crate::writer::record_event`]'s guarantee that pairing credentials never
//! reach disk.
//!
//! # What crosses the boundary
//!
//! A `log::Record` carries four string-shaped channels — `args`, `target`,
//! `module_path`, `file` — plus a numeric `line`. None of the four is a
//! static-only channel: [`log::RecordBuilder`] takes a borrowed `&str` for
//! each of them, and the `log!` macros take a `target:` *expression*, not a
//! literal. So none of them may be forwarded on the assumption that the
//! dependency put a constant there.
//!
//! - **`args` (the message body) is dropped.** It is the record's free-text
//!   channel, written by code this workspace does not review, and nothing
//!   here can tell a harmless sentence from a name, a brand, an identifier or
//!   a credential. Every bridged record carries the fixed
//!   [`REDACTED_MESSAGE`] marker instead. There is no heuristic and no
//!   allowlist, so there is no rule to get wrong.
//! - **`module_path` and `file` are dropped.** They are unbounded strings and
//!   they are redundant: for a record logged without an explicit `target:`,
//!   `log` uses the module path *as* the target, so the target below already
//!   carries the same provenance in a bounded form.
//! - **`line` is forwarded.** It is a `u32`; it has no text channel to carry.
//! - **`target` is reduced to the safe representation below.** It is the one
//!   field the filters read, so dropping it would take `RUST_LOG` target
//!   selection — `RUST_LOG=whatsapp_rust=debug` — with it.
//!
//! # The safe target representation
//!
//! The target that crosses is drawn from a reviewed, finite vocabulary —
//! never from the record. Two constant tables, both read off the locked
//! dependency sources, are that whole vocabulary:
//!
//! - [`TARGET_LITERALS`]: the hand-written `target: "..."` literals in the
//!   pinned `whatsapp-rust` revision (38 at `cbcdd2a6`, `AppState` through
//!   `usync`; the pin contains no computed target expression). An incoming
//!   target crosses only on an exact match, and what crosses is the table's
//!   own constant.
//! - [`TARGET_CRATES`]: the crates the `whatsapp-web` feature activates that
//!   log through the facade *without* an explicit `target:`, so their records
//!   arrive with the module path as the target. Only the crate's reviewed
//!   name crosses: everything after `::` in a module path is still a runtime
//!   string that a hand-built record can abuse (`whatsapp_rust::<jid>` is
//!   charset-clean), so `whatsapp_rust::socket` is reduced to
//!   `whatsapp_rust`. The table's own doc comment carries the per-crate
//!   decision for the whole activated graph.
//!
//! Anything else is replaced *whole* by [`REDACTED_TARGET`] — never
//! truncated and never rewritten character by character, because a partial
//! target leaks the fragments it kept. Fitness is provenance against the
//! tables, not shape: a phone-shaped, secret-shaped or name-shaped runtime
//! value cannot cross no matter which characters it is made of, because the
//! emitted target is by construction one of this file's constants. The
//! price is granularity, not filterability: `RUST_LOG=whatsapp_rust=debug`
//! still addresses every record of the crate, while a per-module directive
//! (`whatsapp_rust::socket=debug`) no longer matches anything, since the
//! reduction happens before the filters look.
//!
//! Note the deliberate contrast with [`zeroclaw_memory::redact`], which is
//! allow-by-default: it rewrites *recognized* patterns in user content the
//! operator opted into storing. Here the input is unreviewed third-party text
//! entering a credential-adjacent sink, so the text channels are not passed.
//!
//! [`zeroclaw_memory::redact`]: https://docs.rs/zeroclaw-memory

use tracing::level_filters::LevelFilter;
use tracing_log::AsTrace;

/// Fixed text substituted for every third-party `log` message body.
///
/// Not a per-token placeholder: the message is never inspected, so this
/// replaces the whole of it every time. Its presence in a record is the
/// signal that a dependency logged at that call site and that the wording was
/// withheld by design rather than lost.
pub(crate) const REDACTED_MESSAGE: &str = "[third-party message redacted]";

/// Fixed text substituted for a `log` target that is not in the safe
/// representation documented at the module level.
///
/// Whole-target replacement, for the same reason the message body is replaced
/// whole: a truncated or character-scrubbed target would still carry the
/// fragments it kept. It appears in neither reviewed table, so it cannot be
/// mistaken for a target a dependency chose and [`safe_target`] is idempotent
/// over it.
pub(crate) const REDACTED_TARGET: &str = "[third-party target redacted]";

/// The hand-written `target: "..."` literals across the whole activated
/// `whatsapp-web` graph at pin `cbcdd2a6`, read off its sources whole — the
/// pin has no computed target expression (`src/handlers/macros.rs` takes
/// `target: $target:literal`, so its expansions are literals too).
/// Byte-sorted for the binary search; a test pins the ordering. An entry here
/// is a claim that this exact string was seen in the reviewed dependency
/// source, so the table only grows by re-reading a pin.
pub(crate) const TARGET_LITERALS: &[&str] = &[
    "AppState",
    "Blocking",
    "Bot/PairCode",
    "Chatstate",
    "ChatstateHandler",
    "Client/AccountSync",
    "Client/Ack",
    "Client/AppState",
    "Client/Business",
    "Client/Contacts",
    "Client/CsToken",
    "Client/DeviceProps",
    "Client/DeviceRegistry",
    "Client/Foo",
    "Client/Group",
    "Client/Groups",
    "Client/IQ",
    "Client/Keepalive",
    "Client/Mex",
    "Client/OfflineResume",
    "Client/OfflineSync",
    "Client/PDO",
    "Client/PairCode",
    "Client/PairTest",
    "Client/Picture",
    "Client/Receipt",
    "Client/Recv",
    "Client/Send",
    "Client/Status",
    "Client/TcToken",
    "Client/UnifiedSession",
    "MessageQueue",
    "Mex",
    "PresenceHandler",
    "TcToken",
    "UnifiedSession",
    "blocklist",
    "usync",
];

/// The crates the `whatsapp-web` feature activates that log through the
/// facade without an explicit `target:`, so their records carry the module
/// path as the target. A module-path target is reduced to its crate's
/// reviewed name; the segments after `::` are runtime strings and never
/// cross.
///
/// Every package in the locked `whatsapp-web` graph at pin `cbcdd2a6`, and
/// the decision made for it. "Default-target calls" counts `log` macro calls
/// with no `target:`; their target is the module path, whose root is the
/// crate's lib name.
///
/// | activated package | default-target calls | decision |
/// |---|---:|---|
/// | `whatsapp-rust` | 540 | **preserve** as `whatsapp_rust` — client, message and socket diagnostics; the crate this bridge exists to recover |
/// | `wacore` | 38 | **preserve** as `wacore` — send path, prekey fetch, device persistence |
/// | `wacore-libsignal` | 35 | **preserve** as `wacore_libsignal` — session and MAC failures; the ones worth reading when decryption breaks |
/// | `whatsapp-rust-tokio-transport` | 8 | **preserve** as `whatsapp_rust_tokio_transport` — websocket dial and read failures |
/// | `wacore-noise` | 2 | **preserve** as `wacore_noise` — frame decode; two `trace!`s, but the same rule as its siblings costs nothing |
/// | `wacore-appstate` | 0 | nothing to decide — all 8 of its calls pass `target: "AppState"`, already in [`TARGET_LITERALS`] |
/// | `wacore-binary`, `waproto`, `whatsapp-rust-ureq-http-client`, `wacore-derive` | 0 | nothing to decide — none depends on `log` |
///
/// Preserving each root rather than folding them into one family keeps the
/// answer to "which component spoke" in the only field that survives the
/// boundary, since `module_path` and `file` are dropped. `EnvFilter` matches
/// a directive's target as a prefix, so `RUST_LOG=whatsapp_rust=debug` picks
/// up `whatsapp_rust_tokio_transport` too and `wacore=debug` picks up
/// `wacore_libsignal` and `wacore_noise`; a directive naming the crate
/// exactly selects just that crate. A test pins both.
///
/// A crate outside this table crosses as [`REDACTED_TARGET`] with its
/// severity and line intact — still more than `master` gives, which is
/// nothing — and widening the table is a one-line change reviewed against a
/// named pin.
pub(crate) const TARGET_CRATES: &[&str] = &[
    "wacore",
    "wacore_libsignal",
    "wacore_noise",
    "whatsapp_rust",
    "whatsapp_rust_tokio_transport",
];

/// The target as it is allowed to cross: a constant from the reviewed
/// tables, or [`REDACTED_TARGET`]. Never a borrow of the record's own
/// value, so the emitted vocabulary is closed by construction.
fn safe_target(target: &str) -> &'static str {
    if let Ok(found) = TARGET_LITERALS.binary_search(&target) {
        return TARGET_LITERALS[found];
    }
    let root = target.split("::").next().unwrap_or(target);
    if let Ok(found) = TARGET_CRATES.binary_search(&root) {
        return TARGET_CRATES[found];
    }
    REDACTED_TARGET
}

/// The `log` logger installed in the process-global slot. Forwards each
/// record into `tracing` through [`tracing_log::format_trace`], which is the
/// same dispatch [`tracing_log::LogTracer`] performs — identical callsite,
/// identical `log.target` / `log.line` normalization — except that the
/// message it carries is always [`REDACTED_MESSAGE`], its target is the safe
/// representation, and `module_path` / `file` are not forwarded at all.
struct RedactingLogBridge;

static BRIDGE: RedactingLogBridge = RedactingLogBridge;

impl log::Log for RedactingLogBridge {
    /// The same contract [`tracing_log::LogTracer`] implements: the global
    /// max level, then the active dispatcher. Without it `log_enabled!`
    /// answers `true` for every level and target — the bridge sets `log`'s
    /// own max level to `Trace` — and a dependency guarding an expensive
    /// diagnostic behind `log_enabled!` builds it even when the `EnvFilter`
    /// is `off`.
    ///
    /// The dispatcher is asked about the *safe* target, because that is the
    /// target the record will actually be dispatched under; asking about the
    /// raw one would let `log_enabled!` disagree with what gets recorded.
    ///
    /// How far that agreement reaches is bounded by `tracing-subscriber`, not
    /// by this bridge. `log` can only ask `Subscriber::enabled`, and a
    /// *per-layer* filter — what [`install_global_subscriber`] uses, since
    /// stderr and the recorded trace filter differently — deliberately does
    /// not answer through it: `Filtered::enabled` returns `true` so the other
    /// layers still get their say, and drops the event later in `on_event`.
    /// So under per-layer filters this answers from the process-wide max
    /// level, which still turns away the common case of a dependency's chatty
    /// `DEBUG`/`TRACE` tiers while the floor is `INFO`, but not a
    /// target-specific exclusion. A globally filtered subscriber is answered
    /// exactly.
    ///
    /// [`install_global_subscriber`]: crate::install_global_subscriber
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.level().as_trace() > LevelFilter::current() {
            return false;
        }
        let metadata = log::Metadata::builder()
            .level(metadata.level())
            .target(safe_target(metadata.target()))
            .build();
        tracing::dispatcher::get_default(|dispatch| dispatch.enabled(&metadata.as_trace()))
    }

    fn log(&self, record: &log::Record<'_>) {
        let target = safe_target(record.target());
        // Ask the subscriber first so a record no layer wants costs nothing
        // to dispatch. `format_trace` repeats this check; doing it here keeps
        // a dependency's chatty `DEBUG`/`TRACE` tiers off the callsite
        // machinery when the filter floor is `INFO`.
        if !self.enabled(
            &log::Metadata::builder()
                .level(record.level())
                .target(target)
                .build(),
        ) {
            return;
        }
        // Rebuilt field by field rather than copied from the incoming record:
        // this list is the whole of what may cross the boundary, so a future
        // `log` release that grows another payload channel (structured
        // key-values, say) cannot ride along unreviewed. `args` is overwritten
        // rather than forwarded, `target` is the safe representation, and
        // `module_path` / `file` are left unset — the builder defaults them to
        // `None` and `tracing_log` then omits the fields entirely.
        let _ = tracing_log::format_trace(
            &log::Record::builder()
                .args(format_args!("{REDACTED_MESSAGE}"))
                .level(record.level())
                .target(target)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Both tables must be byte-sorted or the binary searches silently miss
    /// entries — a miss here fails closed (redaction), but it would still be
    /// a reviewed name going dark.
    #[test]
    fn the_reviewed_tables_are_sorted_for_the_binary_search() {
        for table in [TARGET_LITERALS, TARGET_CRATES] {
            assert!(
                table.windows(2).all(|pair| pair[0] < pair[1]),
                "a reviewed table must be strictly byte-sorted: {table:?}"
            );
        }
    }

    /// The targets the pinned dependency actually emits must cross as their
    /// reviewed constants, or the bridge stops being addressable by
    /// `RUST_LOG` and stops naming which dependency spoke.
    #[test]
    fn reviewed_dependency_targets_cross_as_their_constants() {
        for target in [
            "AppState",
            "Client/PairCode",
            "Client/UnifiedSession",
            "usync",
        ] {
            assert_eq!(
                safe_target(target),
                target,
                "a hand-written literal from the pinned source must cross"
            );
        }
        // Module-path targets reduce to the crate's reviewed name: the crate
        // is provable, the path after `::` is a runtime string.
        assert_eq!(safe_target("whatsapp_rust"), "whatsapp_rust");
        assert_eq!(safe_target("whatsapp_rust::socket"), "whatsapp_rust");
        assert_eq!(safe_target("wacore::send"), "wacore");
        // Which is also what defuses a payload smuggled behind a real crate
        // name — charset-clean, and it still cannot cross.
        assert_eq!(safe_target("whatsapp_rust::9725012345678"), "whatsapp_rust");
    }

    /// The activated `whatsapp-web` graph at pin `cbcdd2a6` has exactly five
    /// crates that log without an explicit `target:`, and each one is a
    /// deliberate `preserve` in the table above. Pinned as a set so a crate
    /// cannot be dropped from the vocabulary — which fails closed, but
    /// closed means a reviewed component goes anonymous — and so a root
    /// cannot be added without re-reading a pin.
    #[test]
    fn every_activated_default_target_crate_crosses_as_its_own_root() {
        assert_eq!(
            TARGET_CRATES,
            [
                "wacore",
                "wacore_libsignal",
                "wacore_noise",
                "whatsapp_rust",
                "whatsapp_rust_tokio_transport",
            ],
            "the reviewed vocabulary must be the activated graph's \
             default-target crates, no more and no less"
        );
        // A bare root, and the module path a default-target call actually
        // produces at each of the sites read off the pin.
        for (emitted, module_path) in [
            ("whatsapp_rust", "whatsapp_rust::message"),
            ("wacore", "wacore::send"),
            ("wacore_libsignal", "wacore_libsignal::protocol::session"),
            ("wacore_noise", "wacore_noise::framing"),
            (
                "whatsapp_rust_tokio_transport",
                "whatsapp_rust_tokio_transport::lib",
            ),
        ] {
            assert_eq!(
                safe_target(emitted),
                emitted,
                "an activated crate's own root must cross"
            );
            assert_eq!(
                safe_target(module_path),
                emitted,
                "a module path must reduce to its crate's reviewed name"
            );
            // And the same reduction defuses a payload smuggled behind that
            // crate's name, for every root in the table rather than one.
            assert_eq!(
                safe_target(&format!("{emitted}::972501234567")),
                emitted,
                "a runtime segment behind a reviewed root must not cross"
            );
        }
        // The rest of the activated graph emits nothing on the default
        // target, so its roots are absent by decision, not by oversight:
        // `wacore_appstate` logs only through `target: "AppState"`, and the
        // other four do not depend on `log` at all.
        for absent in [
            "wacore_appstate",
            "wacore_binary",
            "wacore_derive",
            "waproto",
            "whatsapp_rust_ureq_http_client",
        ] {
            assert_eq!(
                safe_target(absent),
                REDACTED_TARGET,
                "a package with no default-target call must not be in the \
                 vocabulary: {absent}"
            );
        }
        assert_eq!(
            safe_target("AppState"),
            "AppState",
            "`wacore-appstate`'s records cross by their literal instead"
        );
    }

    /// Everything outside the reviewed tables is replaced whole — no
    /// truncation, no character scrubbing — and shape does not help:
    /// every value here fits the old charset rule and each one is exactly
    /// the kind of runtime payload the tables exist to stop.
    #[test]
    fn payload_shaped_targets_are_replaced_whole_whatever_their_shape() {
        let over_long = "a".repeat(4096);
        for target in [
            "",
            "9725012345678",              // bare numeric, phone-shaped
            "sk_live_4eC39HqLyjWDarjtT1", // underscore-separated, secret-shaped
            "Alice",                      // identifier-shaped name
            "Client/Foo2",                // near-miss of a reviewed literal
            "has space",
            "972501234567@s.whatsapp.net",
            "/etc/passwd\n",
            "Zoë Müller",
            over_long.as_str(),
        ] {
            assert_eq!(
                safe_target(target),
                REDACTED_TARGET,
                "a target outside the reviewed tables must be replaced whole: {target:?}"
            );
        }
    }

    /// The replacement is itself outside the representation, so re-running
    /// the rule over an already-replaced target cannot resurrect anything.
    #[test]
    fn the_target_replacement_is_idempotent() {
        assert_eq!(safe_target(REDACTED_TARGET), REDACTED_TARGET);
    }
}
