//! Turn state reported to the terminal itself, over two standard OSC channels.
//!
//! The input bar already shows the turn state, but only to someone looking at
//! this pane. Writing it to the terminal makes it travel: tmux and zellij
//! surface it in their status lines, emulators put it in the tab and the
//! taskbar, and terminal workspace managers read it to decide whether an agent
//! needs attention.
//!
//! Both channels are terminal conventions, not an integration with any one
//! tool. Nothing here knows what is reading it, and no consumer needs ZeroClaw
//! to know about it.
//!
//! **OSC 2 — window title.** Human-facing. Leads with a status glyph, since the
//! glyph survives translation while the verb after it does not.
//!
//! **OSC 9;4 — progress.** Machine-facing, the ConEmu convention that Windows
//! Terminal, WezTerm, and Ghostty implement to drive a busy/error indicator.
//! Its states are semantic rather than decorative, so a reader gets the turn
//! state without matching glyphs or parsing prose in an unknown locale.
//!
//! | Turn state | OSC 2 glyph | OSC 9;4 |
//! |------------|-------------|---------|
//! | idle — waiting for input | `✓` | `0;0` — cleared |
//! | working — turn in flight | `⏳` | `3;0` — indeterminate |
//! | blocked — awaiting approval or input | `⚠` | `4;0` — warning |
//!
//! The terminal is process-global, so the reporter is too: teardown paths and
//! the editor-suspend path need to reach it without threading a handle through
//! every caller.
//!
//! Known limitation: xterm's default title mode decodes titles as ISO-8859-1,
//! where the status glyphs mojibake. Terminals that default to UTF-8 titles
//! (most modern ones) render them correctly. The OSC 9;4 channel is unaffected,
//! being ASCII, which is the other reason state is carried there rather than
//! inferred from the glyph.
//!
//! Title restoration is best-effort. XTPUSHTITLE has no capability response:
//! a terminal may accept the bytes, ignore the title stack, and still honor
//! OSC 2. Zerocode therefore never treats `Write::is_ok()` as proof of support.
//! It records a restore obligation before the first overwrite, pairs every
//! graceful teardown with a neutral title followed by XTPOPTITLE, and lets only
//! one teardown path atomically claim the neutralize and pop obligations. A
//! failed operation re-arms only its own obligation, so a failed neutral write
//! can be retried without popping the saved title twice. A stack-capable
//! terminal restores its saved title; a stack-less terminal keeps the neutral
//! fallback instead of a stale working or blocked title.

use std::io::Write;
use std::sync::{
    Mutex, TryLockError,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use zeroclaw_api::lifecycle::{LifecycleActivity, LifecycleState};

use crate::turn_status::TurnStatus;

/// Terminal tabs are narrow and titles can contain model-influenced tool
/// names. Bound the payload as defense in depth even after controls are removed.
const MAX_TITLE_CHARS: usize = 120;

/// Independent terminal cleanup obligations, claimed together through one
/// atomic state so concurrent teardown paths cannot split neutralize-before-pop
/// ordering between threads.
const CLEANUP_NEUTRALIZE: u8 = 1 << 0;
const CLEANUP_POP: u8 = 1 << 1;
const CLEANUP_ALL: u8 = CLEANUP_NEUTRALIZE | CLEANUP_POP;

/// Status glyph for a turn state. Leading character of the title.
fn glyph(status: &TurnStatus) -> char {
    match status.lifecycle_state() {
        LifecycleState::Idle | LifecycleState::Done => '✓',
        LifecycleState::Working => '⏳',
        LifecycleState::Blocked => '⚠',
    }
}

/// Compose the terminal title for a turn state.
///
/// `agent` is the active agent alias, absent while the agent picker is open.
/// Kept short — terminal tabs truncate aggressively, so the glyph and the alias
/// come first and the verb is the part that gets cut.
pub(crate) fn title_for(status: &TurnStatus, agent: Option<&str>) -> String {
    let glyph = glyph(status);
    let Some(agent) = agent else {
        return format!("{glyph} zerocode");
    };
    match status {
        TurnStatus::Idle => format!("{glyph} {agent}"),
        TurnStatus::WaitingForApproval => format!(
            "{glyph} {agent} — {}",
            crate::i18n::t("zc-chat-status-awaiting-approval")
        ),
        TurnStatus::WaitingForInput => format!(
            "{glyph} {agent} — {}",
            crate::i18n::t("zc-chat-status-awaiting-input")
        ),
        other => match other.verb() {
            Some(verb) => format!("{glyph} {agent} — {verb}"),
            None => format!("{glyph} {agent}"),
        },
    }
}

/// Cleared progress: no indicator at all.
pub(crate) const PROGRESS_CLEARED: &str = "0;0";

/// OSC 9;4 payload for a turn state: `<state>;<progress>`.
///
/// `3` (indeterminate) is the standard "busy, duration unknown" state, which is
/// what an agent turn is. `4` (warning) marks a turn that has stopped and wants
/// the operator — distinct from `2` (error), which would claim the turn failed.
pub(crate) fn progress_for(status: &TurnStatus) -> &'static str {
    progress_for_state(status.lifecycle_state())
}

fn progress_for_state(state: LifecycleState) -> &'static str {
    match state {
        LifecycleState::Idle | LifecycleState::Done => PROGRESS_CLEARED,
        LifecycleState::Working => "3;0",
        LifecycleState::Blocked => "4;0",
    }
}

/// Pick the pane whose state most wants the operator's attention.
///
/// A window hosts more than one agent pane, and the terminal has exactly one
/// title. Reporting the *visible* pane would answer the wrong question — the
/// status is read from outside the window, where what matters is whether
/// anything in here needs a human. Blocked outranks working, which outranks
/// idle; a named pane outranks an unnamed one at equal urgency; remaining ties
/// go to the first pane, which is the primary one.
pub(crate) fn most_urgent<'a>(
    panes: impl IntoIterator<Item = (Option<&'a TurnStatus>, Option<&'a str>)>,
) -> (Option<&'a TurnStatus>, Option<&'a str>) {
    fn rank(pane: (Option<&TurnStatus>, Option<&str>)) -> (u8, bool) {
        let urgency = pane
            .0
            .map_or(0, |status| status.lifecycle_state().attention_rank());
        // Naming breaks an urgency tie only. An unnamed pane knows of no agent,
        // so preferring it would drop a real name from the title for nothing —
        // an idle session on the secondary pane still reads better as
        // `✓ osctest` than as `✓ zerocode`.
        (urgency, pane.1.is_some())
    }

    // Not `max_by_key`: it returns the *last* maximum, which would hand ties to
    // the secondary pane.
    let mut best: Option<(Option<&'a TurnStatus>, Option<&'a str>)> = None;
    for pane in panes {
        if best.is_none_or(|current| rank(pane) > rank(current)) {
            best = Some(pane);
        }
    }
    best.unwrap_or((None, None))
}

/// Emits terminal status, skipping writes when nothing changed.
///
/// A turn's verb changes on most frames while the dots animate, but neither
/// payload is built from them, so a steady turn produces no writes after the
/// first.
#[derive(Default)]
pub(crate) struct StatusReporter {
    last_title: Option<String>,
    last_progress: Option<&'static str>,
}

impl StatusReporter {
    /// Sync both channels from the current turn state. `status` is absent
    /// outside an active chat session (agent picker, dashboard-only use), which
    /// reads as idle.
    fn sync_to(
        &mut self,
        out: &mut impl Write,
        cleanup_needed: &AtomicU8,
        release_epoch: &AtomicUsize,
        expected_release_epoch: usize,
        status: Option<&TurnStatus>,
        agent: Option<&str>,
    ) {
        // Capture happens before the reporter mutex is acquired. A release
        // that won the mutex first invalidates this queued sync so it cannot
        // republish stale status after teardown.
        if release_epoch.load(Ordering::Acquire) != expected_release_epoch {
            self.invalidate();
            return;
        }

        let status = status.unwrap_or(&TurnStatus::Idle);
        let mut title_write_attempted = false;

        let title = title_for(status, agent);
        if self.last_title.as_deref() != Some(title.as_str()) {
            if release_epoch.load(Ordering::Acquire) != expected_release_epoch {
                self.invalidate();
                return;
            }
            title_write_attempted = true;
            // A successful write cannot prove title-stack support, and a
            // failed write may be partial. Record cleanup ownership before
            // sending either sequence, then always pair it with a later pop.
            let previous_cleanup = cleanup_needed.fetch_or(CLEANUP_ALL, Ordering::AcqRel);
            if previous_cleanup & CLEANUP_POP == 0 {
                let _ = push_title(out);
            }
            // Cache only a write that landed: a failed or partial write leaves
            // the terminal showing something else, and caching it as success
            // would suppress the retry that the next transition would make.
            if write_title(out, &title).is_ok() {
                self.last_title = Some(title);
            }
        }

        if release_epoch.load(Ordering::Acquire) != expected_release_epoch {
            self.reconcile_concurrent_release(out, cleanup_needed, title_write_attempted);
            return;
        }

        let progress = progress_for(status);
        if self.last_progress != Some(progress) && write_progress(out, progress).is_ok() {
            self.last_progress = Some(progress);
        }

        if release_epoch.load(Ordering::Acquire) != expected_release_epoch {
            self.reconcile_concurrent_release(out, cleanup_needed, title_write_attempted);
        }
    }

    /// Forget what the terminal is believed to show.
    ///
    /// Handing the terminal to another program (`$EDITOR`) lets it set its own
    /// title, after which the cache no longer describes reality and dedupe
    /// would suppress the correcting write.
    fn invalidate(&mut self) {
        self.last_title = None;
        self.last_progress = None;
    }

    fn release_to(&mut self, out: &mut impl Write, cleanup_needed: &AtomicU8) {
        let _ = write_progress(out, progress_for_state(LifecycleActivity::Finished.state()));
        release_title_obligations(out, cleanup_needed, false);
        self.invalidate();
    }

    /// Repair bytes emitted after a concurrent nonblocking panic cleanup.
    ///
    /// The panic path may already have popped the saved title. In that case a
    /// late title write is neutralized without another pop. If this writer wins
    /// the restore obligation instead, it performs the one pop itself. Either
    /// ordering leaves no stale busy state and permits at most one successful
    /// pop for the saved title.
    fn reconcile_concurrent_release(
        &mut self,
        out: &mut impl Write,
        cleanup_needed: &AtomicU8,
        title_write_attempted: bool,
    ) {
        let _ = write_progress(out, progress_for_state(LifecycleActivity::Finished.state()));
        release_title_obligations(out, cleanup_needed, title_write_attempted);
        self.invalidate();
    }
}

/// The terminal is process-global, and so is what is currently displayed on it.
static REPORTER: Mutex<Option<StatusReporter>> = Mutex::new(None);
/// Cleanup ownership lives outside `REPORTER` so a panic hook can claim it
/// without waiting for the mutex that the panicking write may still hold.
static TITLE_CLEANUP_NEEDED: AtomicU8 = AtomicU8::new(0);
/// Every teardown advances the epoch before touching the reporter mutex. A
/// queued sync aborts when its captured epoch is stale; an in-flight sync
/// neutralizes any bytes that could have landed after nonblocking cleanup.
static RELEASE_EPOCH: AtomicUsize = AtomicUsize::new(0);

fn with_reporter_mutex(
    reporter: &Mutex<Option<StatusReporter>>,
    f: impl FnOnce(&mut StatusReporter),
) {
    let mut guard = match reporter.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(guard.get_or_insert_with(StatusReporter::default));
}

fn with_reporter(f: impl FnOnce(&mut StatusReporter)) {
    with_reporter_mutex(&REPORTER, f);
}

/// Report the current turn state.
pub(crate) fn sync(status: Option<&TurnStatus>, agent: Option<&str>) {
    let expected_release_epoch = RELEASE_EPOCH.load(Ordering::Acquire);
    with_reporter(|r| {
        r.sync_to(
            &mut std::io::stdout(),
            &TITLE_CLEANUP_NEEDED,
            &RELEASE_EPOCH,
            expected_release_epoch,
            status,
            agent,
        );
    });
}

/// Drop the cached view of the terminal after another program may have changed
/// it, so the next sync re-emits rather than deduping against a stale value.
pub(crate) fn invalidate() {
    with_reporter(StatusReporter::invalidate);
}

/// Hand the terminal's status back on the way out.
///
/// Both channels are terminal state, not screen content, so they outlive the
/// process: leaving the alternate screen does not undo them. A zerocode killed
/// mid-turn would otherwise leave `⏳` in the tab and a busy indicator in the
/// taskbar for as long as that terminal lives. Safe to call more than once, and
/// from a panic or signal handler.
pub(crate) fn release() {
    release_reporter_to(
        &REPORTER,
        &TITLE_CLEANUP_NEEDED,
        &RELEASE_EPOCH,
        &mut std::io::stdout(),
    );
}

/// Release through `reporter` without waiting for its mutex.
///
/// A process-wide panic hook runs before unwinding and cannot rely on another
/// worker releasing the lock. A busy reporter therefore gets direct emergency
/// cleanup. The release epoch makes queued writers abort and in-flight writers
/// neutralize any bytes that land after that cleanup.
fn release_reporter_to(
    reporter: &Mutex<Option<StatusReporter>>,
    cleanup_needed: &AtomicU8,
    release_epoch: &AtomicUsize,
    out: &mut impl Write,
) {
    release_epoch.fetch_add(1, Ordering::AcqRel);
    match reporter.try_lock() {
        Ok(mut guard) => guard
            .get_or_insert_with(StatusReporter::default)
            .release_to(out, cleanup_needed),
        Err(TryLockError::Poisoned(poisoned)) => poisoned
            .into_inner()
            .get_or_insert_with(StatusReporter::default)
            .release_to(out, cleanup_needed),
        Err(TryLockError::WouldBlock) => emergency_release_to(out, cleanup_needed),
    }
}

fn emergency_release_to(out: &mut impl Write, cleanup_needed: &AtomicU8) {
    let _ = write_progress(out, progress_for_state(LifecycleActivity::Finished.state()));
    release_title_obligations(out, cleanup_needed, false);
}

/// Claim all currently pending title work in one atomic operation, then re-arm
/// only the operation whose write was unsuccessful. `force_neutralize` covers
/// a title write that landed after a concurrent panic cleanup already claimed
/// the saved-title pop.
fn release_title_obligations(
    out: &mut impl Write,
    cleanup_needed: &AtomicU8,
    force_neutralize: bool,
) {
    let claimed = cleanup_needed.swap(0, Ordering::AcqRel);
    let should_neutralize = force_neutralize || claimed & CLEANUP_NEUTRALIZE != 0;
    let should_pop = claimed & CLEANUP_POP != 0;
    let mut retry = 0;

    if should_neutralize && write_title(out, "zerocode").is_err() {
        retry |= CLEANUP_NEUTRALIZE;
    }
    if should_pop && pop_title(out).is_err() {
        retry |= CLEANUP_POP;
    }
    if retry != 0 {
        cleanup_needed.fetch_or(retry, Ordering::AcqRel);
    }
}

/// Ask the terminal to save its current title (XTPUSHTITLE). There is no
/// capability acknowledgment; teardown pairs any attempted overwrite with a
/// best-effort pop regardless of this write's result.
fn push_title(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(b"\x1b[22;0t")?;
    out.flush()
}

/// Restore the saved title (XTPOPTITLE).
fn pop_title(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(b"\x1b[23;0t")?;
    out.flush()
}

/// Unicode Default_Ignorable format controls that can reorder or hide title
/// text without being `char::is_control()`. Keep this local and dependency-free
/// because the title path needs only a denylist, not full Unicode segmentation.
fn is_format_control(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control() && !is_format_control(*c))
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Write an OSC 2 (set window title) sequence to `out`.
fn write_title(out: &mut impl Write, title: &str) -> std::io::Result<()> {
    // BEL/ESC could terminate or extend the sequence; bidi and other format
    // controls can visually spoof its source even without injecting bytes.
    let sanitized = sanitize_title(title);
    let sequence = format!("\x1b]2;{sanitized}\x07");
    out.write_all(sequence.as_bytes())?;
    out.flush()
}

/// Write an OSC 9;4 (progress) sequence to `out`. `payload` is
/// `<state>;<progress>`.
fn write_progress(out: &mut impl Write, payload: &str) -> std::io::Result<()> {
    let sequence = format!("\x1b]9;4;{payload}\x07");
    out.write_all(sequence.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::{Deref, DerefMut};

    /// Every test drives its own reporter over an in-memory sink. Going through
    /// the module-level functions would write escape sequences to the terminal
    /// running `cargo test` and retitle it.
    #[derive(Default)]
    struct TestReporter {
        inner: StatusReporter,
        cleanup_needed: AtomicU8,
        release_epoch: AtomicUsize,
    }

    impl TestReporter {
        fn sync_to(
            &mut self,
            out: &mut impl Write,
            status: Option<&TurnStatus>,
            agent: Option<&str>,
        ) {
            let expected_release_epoch = self.release_epoch.load(Ordering::Acquire);
            self.inner.sync_to(
                out,
                &self.cleanup_needed,
                &self.release_epoch,
                expected_release_epoch,
                status,
                agent,
            );
        }

        fn release_to(&mut self, out: &mut impl Write) {
            self.inner.release_to(out, &self.cleanup_needed);
        }

        fn needs_restore(&self) -> bool {
            self.cleanup_needed.load(Ordering::Acquire) != 0
        }

        fn cleanup_bits(&self) -> u8 {
            self.cleanup_needed.load(Ordering::Acquire)
        }
    }

    impl Deref for TestReporter {
        type Target = StatusReporter;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl DerefMut for TestReporter {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    fn reporter() -> TestReporter {
        TestReporter::default()
    }

    #[test]
    fn idle_title_is_glyph_and_alias() {
        assert_eq!(title_for(&TurnStatus::Idle, Some("herder")), "✓ herder");
    }

    #[test]
    fn working_title_carries_the_verb() {
        assert_eq!(
            title_for(&TurnStatus::Working, Some("herder")),
            format!("⏳ herder — {}", crate::i18n::t("zc-chat-status-working"))
        );
    }

    #[test]
    fn tool_call_title_names_the_tool() {
        assert_eq!(
            title_for(&TurnStatus::CallingTool("git_diff".into()), Some("herder")),
            format!(
                "⏳ herder — {}",
                crate::i18n::t_args("zc-chat-status-calling-tool", &[("tool", "git_diff")])
            )
        );
    }

    #[test]
    fn approval_title_is_the_warning_glyph() {
        assert_eq!(
            title_for(&TurnStatus::WaitingForApproval, Some("herder")),
            format!(
                "⚠ herder — {}",
                crate::i18n::t("zc-chat-status-awaiting-approval")
            )
        );
    }

    #[test]
    fn elicitation_title_is_the_warning_glyph() {
        assert_eq!(
            title_for(&TurnStatus::WaitingForInput, Some("herder")),
            format!(
                "⚠ herder — {}",
                crate::i18n::t("zc-chat-status-awaiting-input")
            )
        );
    }

    #[test]
    fn without_an_agent_the_title_names_the_app() {
        assert_eq!(title_for(&TurnStatus::Idle, None), "✓ zerocode");
    }

    /// The progress payloads are the contract a reader keys on, so they are
    /// asserted literally rather than through a helper.
    #[test]
    fn progress_states_are_semantic() {
        assert_eq!(progress_for(&TurnStatus::Idle), "0;0");
        assert_eq!(progress_for(&TurnStatus::Working), "3;0");
        assert_eq!(progress_for(&TurnStatus::Thinking), "3;0");
        assert_eq!(progress_for(&TurnStatus::Responding), "3;0");
        assert_eq!(progress_for(&TurnStatus::Cancelling), "3;0");
        assert_eq!(progress_for(&TurnStatus::CallingTool("git".into())), "3;0");
        assert_eq!(progress_for(&TurnStatus::WaitingForApproval), "4;0");
        assert_eq!(progress_for(&TurnStatus::WaitingForInput), "4;0");
    }

    /// First sync saves the terminal's own title before overwriting it, then
    /// emits both channels.
    #[test]
    fn first_sync_pushes_then_writes_both_channels() {
        let mut out = Vec::new();
        reporter().sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "\x1b[22;0t\x1b]2;⏳ herder — {}\x07\x1b]9;4;3;0\x07",
                crate::i18n::t("zc-chat-status-working")
            )
        );
    }

    /// A steady turn must not write on every frame: the animated dots are
    /// excluded from both payloads precisely so this stays silent.
    #[test]
    fn unchanged_state_writes_nothing() {
        let mut r = reporter();
        let mut first = Vec::new();
        r.sync_to(&mut first, Some(&TurnStatus::Working), Some("herder"));
        assert!(!first.is_empty());

        let mut second = Vec::new();
        r.sync_to(&mut second, Some(&TurnStatus::Working), Some("herder"));
        assert!(
            second.is_empty(),
            "repeat sync must be silent, wrote {second:?}"
        );
    }

    /// Progress must not churn while a turn moves between working substates:
    /// they are all one indeterminate turn to anything watching from outside.
    #[test]
    fn progress_is_stable_across_working_substates() {
        let mut r = reporter();
        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));

        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Thinking), Some("herder"));
        let written = String::from_utf8(out).unwrap();
        assert!(
            written.contains(&crate::i18n::t("zc-chat-status-thinking")),
            "title should update"
        );
        assert!(
            !written.contains("\x1b]9;4;"),
            "progress must not be re-emitted: {written:?}"
        );
    }

    /// No active session: the picker and dashboard read as idle rather than
    /// leaving a stale `⏳` in the tab from a finished turn.
    #[test]
    fn absent_status_reads_as_idle() {
        let mut out = Vec::new();
        reporter().sync_to(&mut out, None, None);
        let written = String::from_utf8(out).unwrap();
        assert!(written.contains("\x1b]2;✓ zerocode\x07"));
        assert!(written.contains("\x1b]9;4;0;0\x07"));
    }

    /// Teardown clears progress, writes a neutral fallback, then pops. A
    /// title-stack terminal restores its saved title; a stack-less terminal
    /// ignores the pop but no longer displays stale busy state.
    #[test]
    fn release_clears_progress_and_restores_title() {
        let mut r = reporter();
        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));

        let mut out = Vec::new();
        r.release_to(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b]9;4;0;0\x07\x1b]2;zerocode\x07\x1b[23;0t",
            "release must clear progress, neutralize stack-less terminals, then pop"
        );
    }

    /// Terminals are allowed to ignore XTPUSHTITLE/XTPOPTITLE while still
    /// honoring OSC 2. Model that behavior explicitly: release must replace
    /// the stale working title with the neutral fallback before the ignored
    /// pop, rather than relying on a title stack that does not exist.
    #[test]
    fn stackless_terminal_keeps_neutral_title_after_release() {
        struct StacklessTerminal {
            title: String,
        }

        impl Write for StacklessTerminal {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if let Some(payload) = buf
                    .strip_prefix(b"\x1b]2;")
                    .and_then(|payload| payload.strip_suffix(b"\x07"))
                {
                    self.title = String::from_utf8(payload.to_vec()).unwrap();
                }
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut terminal = StacklessTerminal {
            title: "shell".to_string(),
        };
        let mut r = reporter();
        r.sync_to(&mut terminal, Some(&TurnStatus::Working), Some("herder"));
        assert_eq!(
            terminal.title,
            format!("⏳ herder — {}", crate::i18n::t("zc-chat-status-working"))
        );

        r.release_to(&mut terminal);
        assert_eq!(terminal.title, "zerocode");
    }

    /// Nothing was ever written, so there is no saved title to pop — releasing
    /// must not restore a title this process never touched.
    #[test]
    fn release_without_a_sync_does_not_pop() {
        let mut out = Vec::new();
        reporter().release_to(&mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b]9;4;0;0\x07");
    }

    /// After `$EDITOR` has had the terminal, the cache no longer describes what
    /// is displayed, so the next sync must re-emit rather than dedupe.
    #[test]
    fn invalidate_forces_the_next_sync_to_write() {
        let mut r = reporter();
        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));

        r.invalidate();

        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));
        let written = String::from_utf8(out).unwrap();
        assert!(
            written.contains(&format!(
                "⏳ herder — {}",
                crate::i18n::t("zc-chat-status-working")
            )),
            "title must re-emit"
        );
        assert!(
            written.contains("\x1b]9;4;3;0\x07"),
            "progress must re-emit"
        );
    }

    /// A write that fails must not be cached as displayed, or the retry the
    /// next transition would make gets suppressed.
    #[test]
    fn failed_write_is_not_cached() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("nope"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut r = reporter();
        r.sync_to(&mut Failing, Some(&TurnStatus::Working), Some("herder"));
        assert_eq!(r.last_title, None, "failed title must not be cached");
        assert_eq!(r.last_progress, None, "failed progress must not be cached");

        let mut out = Vec::new();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));
        assert!(
            !out.is_empty(),
            "the next sync must retry after a failed write"
        );
    }

    /// XTPUSHTITLE has no capability response. Even when its write fails, a
    /// later OSC 2 may land, so teardown must still attempt XTPOPTITLE.
    #[test]
    fn failed_push_still_creates_a_restore_obligation() {
        #[derive(Default)]
        struct FailPush {
            bytes: Vec<u8>,
            failed: bool,
        }
        impl Write for FailPush {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if !self.failed && buf == b"\x1b[22;0t" {
                    self.failed = true;
                    return Err(std::io::Error::other("push rejected"));
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut r = reporter();
        let mut out = FailPush::default();
        r.sync_to(&mut out, Some(&TurnStatus::Working), Some("herder"));
        assert!(r.needs_restore());
        assert!(out.bytes.starts_with(b"\x1b]2;"));

        let mut released = Vec::new();
        r.release_to(&mut released);
        assert!(released.ends_with(b"\x1b[23;0t"));
        assert!(!r.needs_restore());
    }

    /// A `write_all` failure can happen after a prefix reached the terminal.
    /// Cache must stay invalid, while cleanup ownership must remain set.
    #[test]
    fn partial_title_write_is_not_cached_and_is_still_restored() {
        #[derive(Default)]
        struct PartialTitle {
            bytes: Vec<u8>,
            fail_next: bool,
            split_done: bool,
        }
        impl Write for PartialTitle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.fail_next {
                    self.fail_next = false;
                    return Err(std::io::Error::other("partial title"));
                }
                if !self.split_done && buf.starts_with(b"\x1b]2;") {
                    let written = 3.min(buf.len());
                    self.bytes.extend_from_slice(&buf[..written]);
                    self.split_done = true;
                    self.fail_next = true;
                    return Ok(written);
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut r = reporter();
        r.sync_to(
            &mut PartialTitle::default(),
            Some(&TurnStatus::Working),
            Some("herder"),
        );
        assert_eq!(r.last_title, None);
        assert!(r.needs_restore());

        let mut released = Vec::new();
        r.release_to(&mut released);
        assert!(released.ends_with(b"\x1b[23;0t"));
    }

    /// A complete byte write followed by a failed flush is still an uncertain
    /// terminal mutation, so it must not be cached as success or skip cleanup.
    #[test]
    fn failed_title_flush_is_not_cached_and_is_still_restored() {
        #[derive(Default)]
        struct FailSecondFlush {
            flushes: usize,
        }
        impl Write for FailSecondFlush {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                if self.flushes == 2 {
                    Err(std::io::Error::other("title flush failed"))
                } else {
                    Ok(())
                }
            }
        }

        let mut r = reporter();
        r.sync_to(
            &mut FailSecondFlush::default(),
            Some(&TurnStatus::Working),
            Some("herder"),
        );
        assert_eq!(r.last_title, None);
        assert!(r.needs_restore());

        let mut released = Vec::new();
        r.release_to(&mut released);
        assert!(released.ends_with(b"\x1b[23;0t"));
    }

    /// A failed pop keeps the obligation live so a second teardown path can
    /// retry instead of silently declaring the title restored.
    #[test]
    fn failed_pop_is_retried_by_the_next_release() {
        #[derive(Default)]
        struct FailFirstPop {
            bytes: Vec<u8>,
            failed: bool,
        }
        impl Write for FailFirstPop {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if !self.failed && buf == b"\x1b[23;0t" {
                    self.failed = true;
                    return Err(std::io::Error::other("pop failed"));
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut r = reporter();
        let mut initial = Vec::new();
        r.sync_to(&mut initial, Some(&TurnStatus::Working), Some("herder"));

        let mut out = FailFirstPop::default();
        r.release_to(&mut out);
        assert!(r.needs_restore(), "failed pop must remain retryable");
        assert_eq!(r.cleanup_bits(), CLEANUP_POP);
        r.release_to(&mut out);
        assert!(!r.needs_restore());
        assert!(out.bytes.ends_with(b"\x1b[23;0t"));
    }

    /// A stack-less terminal needs the neutral OSC 2 fallback even when its
    /// title-stack pop write succeeds. Retrying that failed fallback must not
    /// pop a second title from a stack-capable terminal.
    #[test]
    fn failed_neutral_title_is_retried_without_a_second_pop() {
        #[derive(Default)]
        struct FailFirstNeutralTitle {
            bytes: Vec<u8>,
            failed: bool,
        }

        impl Write for FailFirstNeutralTitle {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if !self.failed && buf == b"\x1b]2;zerocode\x07" {
                    self.failed = true;
                    return Err(std::io::Error::other("neutral title failed"));
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut r = reporter();
        let mut initial = Vec::new();
        r.sync_to(&mut initial, Some(&TurnStatus::Working), Some("herder"));

        let mut out = FailFirstNeutralTitle::default();
        r.release_to(&mut out);
        assert_eq!(r.cleanup_bits(), CLEANUP_NEUTRALIZE);
        assert_eq!(
            out.bytes
                .windows(b"\x1b[23;0t".len())
                .filter(|window| *window == b"\x1b[23;0t")
                .count(),
            1,
            "the saved title should still be popped once"
        );

        r.release_to(&mut out);
        assert_eq!(r.cleanup_bits(), 0);
        assert!(out.bytes.ends_with(b"\x1b]2;zerocode\x07"));
        assert_eq!(
            out.bytes
                .windows(b"\x1b[23;0t".len())
                .filter(|window| *window == b"\x1b[23;0t")
                .count(),
            1,
            "retrying neutralization must not pop an outer title"
        );
    }

    /// Panic cleanup must not wait on the same reporter lock whose critical
    /// section panicked. The fallback is a direct clear + pop pair.
    #[test]
    fn reentrant_release_uses_nonblocking_emergency_cleanup() {
        let reporter = Mutex::new(Some(StatusReporter::default()));
        let cleanup_needed = AtomicU8::new(CLEANUP_ALL);
        let release_epoch = AtomicUsize::new(0);
        let mut out = Vec::new();
        with_reporter_mutex(&reporter, |_| {
            release_reporter_to(&reporter, &cleanup_needed, &release_epoch, &mut out);
        });
        assert_eq!(out, b"\x1b]9;4;0;0\x07\x1b]2;zerocode\x07\x1b[23;0t");
        assert_eq!(cleanup_needed.load(Ordering::Acquire), 0);
        assert_eq!(release_epoch.load(Ordering::Acquire), 1);
    }

    /// The emergency path claims the one restore obligation atomically. Once
    /// it succeeds, the normal teardown that follows unwinding may clear
    /// progress again, but it must not pop an outer terminal title.
    #[test]
    fn emergency_then_normal_release_pops_exactly_once() {
        let reporter = Mutex::new(Some(StatusReporter::default()));
        let cleanup_needed = AtomicU8::new(CLEANUP_ALL);
        let release_epoch = AtomicUsize::new(0);
        let mut out = Vec::new();
        with_reporter_mutex(&reporter, |_| {
            release_reporter_to(&reporter, &cleanup_needed, &release_epoch, &mut out);
        });
        release_reporter_to(&reporter, &cleanup_needed, &release_epoch, &mut out);

        assert_eq!(
            out.windows(b"\x1b[23;0t".len())
                .filter(|window| *window == b"\x1b[23;0t")
                .count(),
            1
        );
    }

    /// A sync can capture its generation before waiting for the reporter lock.
    /// If teardown wins that lock, the queued writer must emit no status after
    /// it finally enters the critical section.
    #[test]
    fn queued_sync_from_before_release_emits_nothing() {
        let mut status = StatusReporter::default();
        let cleanup_needed = AtomicU8::new(0);
        let release_epoch = AtomicUsize::new(1);
        let mut out = Vec::new();

        status.sync_to(
            &mut out,
            &cleanup_needed,
            &release_epoch,
            0,
            Some(&TurnStatus::Working),
            Some("herder"),
        );

        assert!(out.is_empty());
        assert_eq!(cleanup_needed.load(Ordering::Acquire), 0);
    }

    /// A process-wide panic hook cannot wait for a different worker to release
    /// the reporter mutex. Cleanup must return while the title write is held,
    /// and the writer must neutralize its late bytes without a second pop.
    #[test]
    fn cross_thread_release_is_prompt_and_reconciles_a_late_write() {
        struct BlockingTitleSink {
            bytes: Vec<u8>,
            blocked_tx: Option<std::sync::mpsc::Sender<()>>,
            resume_rx: std::sync::mpsc::Receiver<()>,
        }

        impl Write for BlockingTitleSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if buf.starts_with(b"\x1b]2;")
                    && let Some(blocked_tx) = self.blocked_tx.take()
                {
                    blocked_tx.send(()).unwrap();
                    self.resume_rx.recv().unwrap();
                }
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let reporter = std::sync::Arc::new(Mutex::new(Some(StatusReporter::default())));
        let cleanup_needed = std::sync::Arc::new(AtomicU8::new(0));
        let release_epoch = std::sync::Arc::new(AtomicUsize::new(0));
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (writer_done_tx, writer_done_rx) = std::sync::mpsc::channel();

        let writer_reporter = std::sync::Arc::clone(&reporter);
        let writer_cleanup = std::sync::Arc::clone(&cleanup_needed);
        let writer_epoch = std::sync::Arc::clone(&release_epoch);
        let writer = std::thread::spawn(move || {
            let expected_release_epoch = writer_epoch.load(Ordering::Acquire);
            let mut sink = BlockingTitleSink {
                bytes: Vec::new(),
                blocked_tx: Some(locked_tx),
                resume_rx,
            };
            with_reporter_mutex(&writer_reporter, |status| {
                status.sync_to(
                    &mut sink,
                    &writer_cleanup,
                    &writer_epoch,
                    expected_release_epoch,
                    Some(&TurnStatus::Working),
                    Some("herder"),
                );
            });
            writer_done_tx.send(sink.bytes).unwrap();
        });
        locked_rx.recv().unwrap();

        let release_reporter = std::sync::Arc::clone(&reporter);
        let release_cleanup = std::sync::Arc::clone(&cleanup_needed);
        let release_epoch = std::sync::Arc::clone(&release_epoch);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let release = std::thread::spawn(move || {
            let mut out = Vec::new();
            release_reporter_to(
                &release_reporter,
                &release_cleanup,
                &release_epoch,
                &mut out,
            );
            done_tx.send(out).unwrap();
        });

        let release_out = match done_rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(out) => out,
            Err(error) => {
                resume_tx.send(()).unwrap();
                writer.join().unwrap();
                release.join().unwrap();
                panic!("panic cleanup blocked on a held reporter mutex: {error}");
            }
        };
        assert!(release_out.ends_with(b"\x1b[23;0t"));

        resume_tx.send(()).unwrap();
        let writer_out = writer_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("the in-flight writer should reconcile after release");

        writer.join().unwrap();
        release.join().unwrap();
        assert!(
            writer_out.ends_with(b"\x1b]2;zerocode\x07"),
            "a late title write must be neutralized: {writer_out:?}"
        );
        let pop = b"\x1b[23;0t";
        let pop_count = release_out.windows(pop.len()).filter(|w| *w == pop).count()
            + writer_out.windows(pop.len()).filter(|w| *w == pop).count();
        assert_eq!(pop_count, 1, "concurrent cleanup must pop exactly once");
        assert_eq!(cleanup_needed.load(Ordering::Acquire), 0);
    }

    /// A blocked pane wins even when it is not the visible one — the whole
    /// point is to answer "does anything in this window need me?".
    #[test]
    fn most_urgent_prefers_blocked_then_working() {
        let blocked = TurnStatus::WaitingForApproval;
        let asking = TurnStatus::WaitingForInput;
        let working = TurnStatus::Working;
        let idle = TurnStatus::Idle;

        let (status, agent) =
            most_urgent([(Some(&idle), Some("chat")), (Some(&blocked), Some("code"))]);
        assert!(matches!(status, Some(TurnStatus::WaitingForApproval)));
        assert_eq!(agent, Some("code"));

        let (status, agent) = most_urgent([
            (Some(&working), Some("chat")),
            (Some(&asking), Some("code")),
        ]);
        assert!(matches!(status, Some(TurnStatus::WaitingForInput)));
        assert_eq!(agent, Some("code"));

        let (status, agent) =
            most_urgent([(Some(&idle), Some("chat")), (Some(&working), Some("code"))]);
        assert!(matches!(status, Some(TurnStatus::Working)));
        assert_eq!(agent, Some("code"));

        // A pane with no session must not outrank a working one.
        let (status, agent) = most_urgent([(None, None), (Some(&working), Some("code"))]);
        assert!(matches!(status, Some(TurnStatus::Working)));
        assert_eq!(agent, Some("code"));
    }

    /// Ties go to the primary pane, so an idle window keeps naming the pane the
    /// operator thinks of as theirs rather than flapping between two.
    #[test]
    fn most_urgent_breaks_ties_toward_the_first_pane() {
        let idle = TurnStatus::Idle;
        let (_, agent) = most_urgent([(Some(&idle), Some("chat")), (Some(&idle), Some("code"))]);
        assert_eq!(agent, Some("chat"));

        let working = TurnStatus::Working;
        let (_, agent) = most_urgent([
            (Some(&working), Some("chat")),
            (Some(&working), Some("code")),
        ]);
        assert_eq!(agent, Some("chat"));
    }

    /// Observed live: finishing a turn in the Code pane with no Chat agent
    /// selected settled the title to `✓ zerocode` instead of `✓ osctest`,
    /// because both panes ranked idle and the tie went to the nameless primary.
    #[test]
    fn most_urgent_keeps_a_named_pane_over_a_nameless_tie() {
        let idle = TurnStatus::Idle;
        let (_, agent) = most_urgent([(None, None), (Some(&idle), Some("osctest"))]);
        assert_eq!(agent, Some("osctest"));

        // Same rank, both named: the primary still wins, so this does not
        // reorder panes that both have something to say.
        let (_, agent) = most_urgent([(Some(&idle), Some("chat")), (Some(&idle), Some("code"))]);
        assert_eq!(agent, Some("chat"));
    }

    /// Naming breaks ties; it must never outrank urgency itself.
    #[test]
    fn most_urgent_ranks_urgency_above_naming() {
        let idle = TurnStatus::Idle;
        let working = TurnStatus::Working;
        let blocked = TurnStatus::WaitingForApproval;

        // A nameless working pane still beats a named idle one.
        let (status, agent) = most_urgent([(Some(&idle), Some("chat")), (Some(&working), None)]);
        assert!(matches!(status, Some(TurnStatus::Working)));
        assert_eq!(agent, None);

        // ...and a nameless blocked pane still beats a named working one.
        let (status, _) = most_urgent([(Some(&working), Some("chat")), (Some(&blocked), None)]);
        assert!(matches!(status, Some(TurnStatus::WaitingForApproval)));
    }

    /// A BEL inside the alias would terminate the OSC string early and leave
    /// the remainder to be read as terminal input.
    #[test]
    fn control_characters_are_stripped_before_emission() {
        let mut out = Vec::new();
        write_title(&mut out, &title_for(&TurnStatus::Idle, Some("her\x07der"))).unwrap();
        assert_eq!(out, "\x1b]2;✓ herder\x07".as_bytes());
    }

    #[test]
    fn bidi_and_other_format_controls_are_stripped_from_titles() {
        assert_eq!(sanitize_title("safe\u{202e}txt\u{200d}"), "safetxt");
    }

    #[test]
    fn title_payload_is_bounded_without_splitting_unicode() {
        let input = "界".repeat(MAX_TITLE_CHARS + 20);
        let sanitized = sanitize_title(&input);
        assert_eq!(sanitized.chars().count(), MAX_TITLE_CHARS);
        assert!(sanitized.chars().all(|c| c == '界'));
    }
}
