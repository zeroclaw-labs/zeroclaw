//! Per-token Telegram Bot API flood-control state.
//!
//! Telegram answers excess requests with HTTP 429 and a `retry_after` hint
//! (`parameters.retry_after` in the JSON envelope, or `retry after <N>` in
//! the human-readable description). Firing again inside that window escalates
//! the token's penalty, so this module keeps one shared "blocked until"
//! instant per channel instance.
//!
//! Gated surface: the channel's text traffic - `sendMessage` (final answers,
//! draft creation, approval prompts), `editMessageText` (draft updates and
//! finalize), and `sendChatAction` (typing) - goes through
//! [`post_bot_api_json`]. Media uploads, `deleteMessage`, `answerCallbackQuery`,
//! and the model picker's markup edits still post directly; they are not
//! covered by this gate.
//!
//! State-source justification (AGENTS.md): `blocked_until` and
//! `penalty_until` are newly created runtime state - the only local mirror of
//! Telegram's own server-side limiter. They duplicate no config key and no
//! other runtime field.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Fallback window when a 429 body carries no parseable `retry_after`.
/// Telegram's Bot API contract always sends one, so this only covers
/// proxy-generated or truncated answers.
pub(crate) const FALLBACK_RETRY_AFTER_SECS: u64 = 5;

/// Upper bound of the jitter `note_rate_limited` adds on top of the server
/// penalty. The jittered deadline can therefore outlive the penalty itself by
/// at most this much, which lets waiters with a fully spent budget tell "the
/// penalty equaled the budget and only the jitter tail remains" apart from "the
/// penalty genuinely outlasts the budget".
pub(crate) const MAX_WINDOW_JITTER: Duration = Duration::from_secs(2);

/// Delivery class of a Bot API call: whether the caller must get the request
/// through (waiting out a flood-control window, bounded by
/// `rate_limit_max_wait_secs`) or may skip it entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BotApiClass {
    /// Cosmetic traffic (draft edits, typing indicators): skip when blocked,
    /// the next scheduled attempt replaces it.
    Droppable,
    /// User-visible payloads (final answers, approval prompts): wait out the
    /// window and retry once instead of dropping the message.
    Deliver,
}

/// Outcome of a gate check against the shared flood-control window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitGate {
    /// No active window; the call may proceed.
    Clear,
    /// Inside a window; carries the remaining wait.
    Blocked(Duration),
}

/// One coherent observation of the jittered window and server penalty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimitSnapshot {
    pub(crate) gate: RateLimitGate,
    pub(crate) penalty_remaining: Duration,
}

/// Why a gated Bot API call produced no usable response. Every variant means
/// "nothing was delivered and the shared window already accounts for it", so
/// callers must stop their formatting/resume/resend fallbacks - a 429 is not
/// a formatting failure. Droppable-class callers log at DEBUG and move on.
#[derive(Debug)]
pub(crate) enum TelegramGateError {
    /// Pre-send gate: the token is inside a flood-control window, so a
    /// Droppable call was skipped without any network call.
    Skipped { remaining: Duration },
    /// The call reached Telegram and was answered 429; the window was noted
    /// and the body consumed to harvest `retry_after`. For a Deliver call this
    /// marks the single permitted retry also answering 429 (its own, possibly
    /// longer, delay is the one recorded).
    FloodLimited { retry_after_secs: u64 },
    /// A Deliver call spent its whole caller-side wait budget and the server
    /// penalty was still running when it re-checked - the penalty outlasts
    /// the budget, or a concurrent 429 extended the window while this caller
    /// slept. No (re)send happened.
    BudgetExhausted { remaining: Duration },
}

impl std::fmt::Display for TelegramGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skipped { remaining } => write!(
                f,
                "Telegram Bot API call skipped: token blocked for {}s more",
                remaining.as_secs()
            ),
            Self::FloodLimited { retry_after_secs } => write!(
                f,
                "Telegram Bot API answered 429 Too Many Requests; blocked for {retry_after_secs}s"
            ),
            Self::BudgetExhausted { remaining } => write!(
                f,
                "Telegram Bot API wait budget expired with the token still blocked for {}s more; \
                 nothing was sent",
                remaining.as_secs()
            ),
        }
    }
}

impl std::error::Error for TelegramGateError {}

/// Canonical pair of maximum deadlines observed from Telegram responses.
#[derive(Debug, Default)]
struct DeadlineState {
    blocked_until: Option<Instant>,
    // The greatest unjittered server penalty is independent of which response
    // established the longest jittered window, but both publish atomically.
    penalty_until: Option<Instant>,
}

/// Shared per-token flood-control window. Clones of one channel instance
/// observe the same deadline pair, whichever chat the request targets.
#[derive(Debug, Clone, Default)]
pub(crate) struct TelegramRateLimiter {
    deadlines: Arc<Mutex<DeadlineState>>,
    #[cfg(test)]
    publication_pause: Arc<Mutex<Option<PublicationPause>>>,
}

#[cfg(test)]
#[derive(Debug)]
struct PublicationPause {
    reached: std::sync::mpsc::Sender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

impl TelegramRateLimiter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Block the token until `retry_after_secs` (plus up to
    /// [`MAX_WINDOW_JITTER`] of jitter, so several chats blocked by the same
    /// 429 do not all resume on the same tick) from now. Jitter derives from
    /// the wall clock's sub-second nanos; no `rand` dependency is warranted
    /// for a 3-way pick.
    ///
    /// Concurrent 429 responses can arrive in either order, so the new
    /// deadline only replaces the active one when it is *later*: a 1s answer
    /// landing after a 30s one must not shorten the block. A delay so large
    /// it cannot be represented as an `Instant` is bogus (Telegram penalties
    /// are seconds to minutes); it is rejected outright - the active deadline
    /// stands - rather than clamped into permission to send sooner.
    ///
    /// The jittered deadline and the unjittered server penalty are tracked
    /// separately: whichever 429 carries the later jittered deadline gates
    /// the sends, while the greatest penalty deadline governs
    /// [`Self::snapshot`], so a shorter penalty padded with jitter
    /// can advance the gate's deadline without hiding a longer penalty that
    /// is still running.
    pub(crate) fn note_rate_limited(&self, retry_after_secs: u64) {
        let jitter_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()) % (MAX_WINDOW_JITTER.as_secs() + 1))
            .unwrap_or(0);
        self.note_rate_limited_with_jitter(retry_after_secs, jitter_secs);
    }

    /// Test-deterministic core of [`Self::note_rate_limited`]: the jitter is
    /// supplied by the caller instead of the wall clock, so overlapping-429
    /// interleavings with exact, unequal jitter can be exercised as
    /// regressions.
    fn note_rate_limited_with_jitter(&self, retry_after_secs: u64, jitter_secs: u64) {
        let jitter_secs = jitter_secs.min(MAX_WINDOW_JITTER.as_secs());
        let now = Instant::now();
        let new_deadline = retry_after_secs
            .checked_add(jitter_secs)
            .map(Duration::from_secs)
            .and_then(|window| now.checked_add(window));
        let penalty_deadline = now.checked_add(Duration::from_secs(retry_after_secs));
        let Some((new_deadline, penalty_deadline)) = new_deadline.zip(penalty_deadline) else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"retry_after_secs": retry_after_secs})),
                "Telegram 429 retry_after is not representable as a deadline; keeping the active window"
            );
            return;
        };
        let mut guard = self.deadlines.lock();
        if guard
            .blocked_until
            .is_none_or(|active| active < new_deadline)
        {
            guard.blocked_until = Some(new_deadline);
        }
        #[cfg(test)]
        self.pause_publication_for_test();
        if guard
            .penalty_until
            .is_none_or(|active| active < penalty_deadline)
        {
            guard.penalty_until = Some(penalty_deadline);
        }
    }

    #[cfg(test)]
    fn pause_publication_for_test(&self) {
        let pause = self.publication_pause.lock().take();
        if let Some(pause) = pause {
            pause
                .reached
                .send(())
                .expect("publication controller remains live");
            pause
                .resume
                .recv_timeout(Duration::from_secs(10))
                .expect("publication controller releases the updater");
        }
    }

    /// Read or expire both deadlines under one lock and one time sample.
    /// The server penalty excludes jitter, so a penalty equal to the caller's
    /// wait budget can still be admitted once only the jitter tail remains.
    pub(crate) fn snapshot(&self) -> RateLimitSnapshot {
        let mut guard = self.deadlines.lock();
        let now = Instant::now();
        let gate = match guard.blocked_until {
            Some(until) if until > now => RateLimitGate::Blocked(until.duration_since(now)),
            _ => {
                *guard = DeadlineState::default();
                RateLimitGate::Clear
            }
        };
        let penalty_remaining = guard
            .penalty_until
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(now));
        RateLimitSnapshot {
            gate,
            penalty_remaining,
        }
    }

    pub(crate) fn check(&self) -> RateLimitGate {
        self.snapshot().gate
    }
}

/// Harvest `retry_after` (seconds) from a 429 body: first the structured
/// `parameters.retry_after` of the Bot API JSON envelope, then a
/// case-insensitive `retry after <N>` scan of the raw text (the description
/// Telegram writes when the envelope is absent or truncated).
pub(crate) fn parse_retry_after(body: &str) -> Option<u64> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(secs) = value
            .get("parameters")
            .and_then(|p| p.get("retry_after"))
            .and_then(serde_json::Value::as_u64)
    {
        return Some(secs);
    }

    let lower = body.to_ascii_lowercase();
    let needle = "retry after";
    let mut offset = 0;
    loop {
        let found = lower.get(offset..)?.find(needle)?;
        let needle_end = offset + found + needle.len();
        let rest = body.get(needle_end..)?.trim_start();
        let digit_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_len > 0 {
            return rest.get(..digit_len).and_then(|d| d.parse::<u64>().ok());
        }
        offset = needle_end;
    }
}

/// Wait out an active flood-control window for a Deliver-class call, drawing
/// down the caller's `budget` - the remaining slice of the operation's single
/// `max_wait` allowance, shared by the pre-send wait and the wait before the
/// retry. The loop keeps waiting and re-checking until the gate clears or the
/// budget runs out: a window extended by a concurrent 429 while this caller
/// sleeps is honored (never sent into) as long as budget remains. The budget
/// bounds how long *this caller* waits, never the server deadline: when it is
/// spent and the penalty is still running, the wait ends with
/// [`TelegramGateError::BudgetExhausted`] instead of an early send.
///
/// Boundary case: the gate's deadline carries the jitter
/// [`TelegramRateLimiter::note_rate_limited`] added on top of the server
/// penalty, so a spent budget can leave up to that jitter on the clock even
/// though the penalty itself already ended. The exhaustion decision therefore
/// compares the *penalty* remaining against the budget, not the jittered
/// remaining wait: a penalty that merely equals the budget is waited out and
/// the call proceeds; a penalty that outlives the budget still fails.
///
/// Also used by the final-reply `sendMessage` path, which keeps its own
/// attempt accounting: it shares this loop (and so the same rechecking
/// semantics) instead of duplicating the wait.
pub(crate) async fn wait_out_window(
    limiter: &TelegramRateLimiter,
    budget: &mut Duration,
) -> Result<(), TelegramGateError> {
    loop {
        let snapshot = limiter.snapshot();
        match snapshot.gate {
            RateLimitGate::Clear => return Ok(()),
            RateLimitGate::Blocked(remaining) => {
                if budget.is_zero() {
                    // The deadline outruns the server penalty by the jitter
                    // the note added; a spent budget with the penalty itself
                    // already over means the penalty fit the budget exactly.
                    if snapshot.penalty_remaining.is_zero() {
                        return Ok(());
                    }
                    return Err(TelegramGateError::BudgetExhausted { remaining });
                }
                let wait = remaining.min(*budget);
                let slept = Instant::now();
                tokio::time::sleep(wait).await;
                *budget = budget.saturating_sub(slept.elapsed());
            }
        }
    }
}

/// POST `body` to a Bot API `url` through the shared flood-control gate.
///
/// - Pre-send, a Droppable call inside the window fails with
///   [`TelegramGateError::Skipped`] without any network call; a Deliver call
///   waits out the window (bounded by `max_wait`, re-checking after each
///   sleep so a mid-wait extension is honored while budget remains) - a spent
///   budget with the gate still blocked means
///   [`TelegramGateError::BudgetExhausted`], no request.
/// - On a 429 answer the window is noted from that response's own
///   `retry_after`. Droppable calls then fail with
///   [`TelegramGateError::FloodLimited`] (the body was consumed for parsing,
///   so no response survives to return); Deliver calls wait the window out
///   from the *remaining* budget - the pre-send wait and this wait share one
///   per-operation allowance, so a call never waits longer than `max_wait`
///   in total - and retry once. A retry that also 429s contributes its own -
///   possibly longer - `retry_after` to the window and fails with
///   [`TelegramGateError::FloodLimited`]: the caller's formatting/resume
///   fallbacks must stop, a 429 is not a formatting failure.
///
/// Any other status - success or failure - is returned untouched, and
/// transport errors propagate.
pub(crate) async fn post_bot_api_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    class: BotApiClass,
    limiter: &TelegramRateLimiter,
    max_wait: Duration,
) -> anyhow::Result<reqwest::Response> {
    // One caller-side allowance covers the whole operation; Droppable calls
    // never draw on it.
    let mut budget = max_wait;
    match class {
        BotApiClass::Droppable => {
            if let RateLimitGate::Blocked(remaining) = limiter.check() {
                return Err(TelegramGateError::Skipped { remaining }.into());
            }
        }
        BotApiClass::Deliver => wait_out_window(limiter, &mut budget).await?,
    }

    let resp = client.post(url).json(body).send().await?;
    if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(resp);
    }

    let retry_after_secs = parse_retry_after(&resp.text().await.unwrap_or_default())
        .unwrap_or(FALLBACK_RETRY_AFTER_SECS);
    limiter.note_rate_limited(retry_after_secs);

    match class {
        BotApiClass::Droppable => Err(TelegramGateError::FloodLimited { retry_after_secs }.into()),
        BotApiClass::Deliver => {
            wait_out_window(limiter, &mut budget).await?;
            let retry = client.post(url).json(body).send().await?;
            if retry.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Ok(retry);
            }
            // The single permitted retry was flood-limited again: harvest the
            // new delay into the shared window and stop - returning the 429
            // response would let the caller's formatting fallback fire into
            // the still-active window.
            let retry_after_secs = parse_retry_after(&retry.text().await.unwrap_or_default())
                .unwrap_or(FALLBACK_RETRY_AFTER_SECS);
            limiter.note_rate_limited(retry_after_secs);
            Err(TelegramGateError::FloodLimited { retry_after_secs }.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_error(err: &anyhow::Error) -> &TelegramGateError {
        err.downcast_ref::<TelegramGateError>()
            .expect("expected a TelegramGateError")
    }

    struct PausedPublisher {
        resume: Option<std::sync::mpsc::Sender<()>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl PausedPublisher {
        fn finish(mut self) {
            self.resume.take().unwrap().send(()).unwrap();
            self.worker.take().unwrap().join().unwrap();
        }
    }

    impl Drop for PausedPublisher {
        fn drop(&mut self) {
            if let Some(resume) = self.resume.take() {
                let _ = resume.send(());
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deliver_cannot_observe_half_published_extended_penalty() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
            )
            .mount(&server)
            .await;

        let limiter = TelegramRateLimiter::new();
        // The old penalty is spent, but its jitter still blocks. Pause a real
        // extending update after the gate field is written and before its
        // longer server penalty is published.
        limiter.note_rate_limited_with_jitter(0, 2);
        let (reached, reached_rx) = std::sync::mpsc::channel();
        let (resume, resume_rx) = std::sync::mpsc::channel();
        *limiter.publication_pause.lock() = Some(PublicationPause {
            reached,
            resume: resume_rx,
        });
        let publisher = limiter.clone();
        let paused = PausedPublisher {
            resume: Some(resume),
            worker: Some(std::thread::spawn(move || {
                publisher.note_rate_limited_with_jitter(30, 0);
            })),
        };
        reached_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the actual updater must reach the publication pause");

        let waiter = limiter.clone();
        let url = format!("{}/bottoken/sendMessage", server.uri());
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let mut request = zeroclaw_spawn::spawn!(async move {
            let _ = started.send(());
            post_bot_api_json(
                &reqwest::Client::new(),
                &url,
                &serde_json::json!({"chat_id": "1", "text": "held reply"}),
                BotApiClass::Deliver,
                &waiter,
                Duration::ZERO,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("the real sender must start")
            .unwrap();
        let early = tokio::time::timeout(Duration::from_secs(2), &mut request).await;
        // Release and join even on the negative variant before asserting, so
        // an observed early POST cannot strand a publisher or held lock.
        paused.finish();
        let result = match early {
            Ok(result) => result.unwrap(),
            Err(_) => tokio::time::timeout(Duration::from_secs(5), request)
                .await
                .expect("sender must finish once the update is published")
                .unwrap(),
        };
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a spent-budget sender must not POST through a half-published extended penalty"
        );
        let err = result.expect_err("the extended server penalty must exhaust the zero budget");
        assert!(matches!(
            gate_error(&err),
            TelegramGateError::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn parse_retry_after_reads_json_envelope() {
        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 5","parameters":{"retry_after":5}}"#;
        assert_eq!(parse_retry_after(body), Some(5));
    }

    #[test]
    fn parse_retry_after_falls_back_to_description_text() {
        // JSON envelope without `parameters`: the description's own hint.
        let body =
            r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 12"}"#;
        assert_eq!(parse_retry_after(body), Some(12));
        // Bare proxy text, no JSON at all.
        assert_eq!(
            parse_retry_after("Too Many Requests: retry after 7"),
            Some(7)
        );
    }

    #[test]
    fn parse_retry_after_is_case_insensitive() {
        assert_eq!(
            parse_retry_after("Too many requests: RETRY AFTER 9"),
            Some(9)
        );
    }

    #[test]
    fn parse_retry_after_returns_none_without_hint() {
        assert_eq!(
            parse_retry_after(r#"{"ok":false,"description":"Not Found"}"#),
            None
        );
        assert_eq!(parse_retry_after("service unavailable"), None);
    }

    #[test]
    fn gate_is_clear_initially_and_blocks_on_note() {
        let limiter = TelegramRateLimiter::new();
        assert_eq!(limiter.check(), RateLimitGate::Clear);

        limiter.note_rate_limited(30);
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(remaining > Duration::from_secs(29));
                assert!(remaining <= Duration::from_secs(33));
            }
            RateLimitGate::Clear => panic!("expected blocked after note_rate_limited"),
        }
    }

    #[test]
    fn gate_clears_expired_window() {
        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_millis(40));

        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(remaining <= Duration::from_millis(40))
            }
            RateLimitGate::Clear => panic!("expected blocked while window is live"),
        }

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(limiter.check(), RateLimitGate::Clear);
        assert!(
            limiter.deadlines.lock().blocked_until.is_none(),
            "an expired window must be cleared, not kept"
        );
    }

    #[test]
    fn clones_share_one_window() {
        let limiter = TelegramRateLimiter::new();
        let clone = limiter.clone();
        limiter.note_rate_limited(30);
        assert!(matches!(clone.check(), RateLimitGate::Blocked(_)));
    }

    #[test]
    fn note_rate_limited_keeps_the_longer_active_deadline() {
        // In-flight requests can be answered out of order: a 30s penalty
        // followed by a 1s one must not shorten the block to ~1s.
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited(30);
        limiter.note_rate_limited(1);
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(
                    remaining > Duration::from_secs(29),
                    "the 1s response must not shorten the active 30s window: {remaining:?}"
                );
            }
            RateLimitGate::Clear => panic!("expected the 30s window to survive a 1s note"),
        }
    }

    #[test]
    fn note_rate_limited_extends_a_shorter_active_deadline() {
        // Escalation: a later, longer penalty replaces the short one.
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited(5);
        limiter.note_rate_limited(30);
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(remaining > Duration::from_secs(29));
            }
            RateLimitGate::Clear => panic!("expected blocked after escalation note"),
        }
    }

    #[test]
    fn note_rate_limited_rejects_an_unrepresentable_delay() {
        // A bogus `retry_after` that cannot become an `Instant` must not be
        // clamped into permission to send sooner: the active window stands.
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited(30);
        limiter.note_rate_limited(u64::MAX);
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(
                    remaining > Duration::from_secs(29),
                    "the unrepresentable delay must be rejected, not clamped: {remaining:?}"
                );
            }
            RateLimitGate::Clear => panic!("expected the prior window to survive a bogus note"),
        }
    }

    #[test]
    fn overlapping_429_jitter_cannot_hide_the_longer_penalty() {
        // A shorter penalty padded with enough jitter takes over the gate's
        // deadline, but the longer penalty that arrived first must still
        // govern `penalty_remaining`: a caller's budget is measured against
        // the server truth, not against whichever deadline won the jitter.
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited_with_jitter(30, 0);
        limiter.note_rate_limited_with_jitter(29, 2);
        // The jittered gate deadline (29s + 2s) now exceeds the first
        // window's 30s, so the gate itself is the new deadline…
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => assert!(
                remaining > Duration::from_secs(28),
                "the gate keeps the later jittered deadline: {remaining:?}"
            ),
            RateLimitGate::Clear => panic!("expected the replaced jittered window to hold"),
        }
        // …but the unjittered penalty still runs for the full 30s.
        let penalty = limiter.snapshot().penalty_remaining;
        assert!(
            penalty > Duration::from_secs(29),
            "a 2s-jittered 29s note must not shorten the 30s penalty: {penalty:?}"
        );
    }

    #[test]
    fn penalty_equal_to_budget_is_admitted_exactly() {
        // Exact-budget control: the penalty a budget is compared against is
        // the server penalty itself, unaffected by the jitter on the gate, so
        // a 30s penalty against a 30s budget is waited out and sent (not
        // refused as outliving the budget).
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited_with_jitter(30, 2);
        let penalty = limiter.snapshot().penalty_remaining;
        assert!(
            penalty > Duration::from_secs(29),
            "the penalty itself is the full 30s, jitter excluded: {penalty:?}"
        );
        assert!(
            penalty <= Duration::from_secs(30),
            "the penalty cannot exceed the noted retry_after: {penalty:?}"
        );
    }

    #[tokio::test]
    async fn deliver_waits_out_window_and_sends_when_within_budget() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_millis(700));

        let started = Instant::now();
        let resp = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(30),
        )
        .await
        .expect("a window within the budget is waited out, then sent");

        assert!(resp.status().is_success());
        assert!(
            started.elapsed() >= Duration::from_millis(700),
            "the send must not fire before the window expires: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn deliver_budget_expiry_makes_no_request() {
        // Penalty 30s, budget 1s: after the capped wait the gate is still
        // blocked, so nothing may be sent.
        let mock_server = wiremock::MockServer::start().await;

        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited_with_jitter(30, 0);

        let started = Instant::now();
        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(1),
        )
        .await;

        let err = match result {
            Ok(_) => panic!("a 1s budget must not authorize a send into a 30s window"),
            Err(err) => err,
        };
        assert!(
            matches!(gate_error(&err), TelegramGateError::BudgetExhausted { .. }),
            "expected BudgetExhausted, got: {err}"
        );
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "the caller waits its budget before giving up: {:?}",
            started.elapsed()
        );
        assert!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "no request may fire while the window is active"
        );
    }

    #[tokio::test]
    async fn deliver_keeps_waiting_when_window_extended_within_budget() {
        // The window is short when this caller starts waiting, but a
        // concurrent 429 extends it mid-wait. With most of the budget still
        // remaining, the caller must keep waiting and re-checking - not give
        // up after one sleep - and the send lands once the extended window
        // clears, well within budget.
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let mock_server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_millis(600));

        let extender = limiter.clone();
        let extend = zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            extender.note_rate_limited(2);
        });

        let started = Instant::now();
        let resp = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(30),
        )
        .await;
        extend.await.expect("extender task");

        assert!(
            resp.expect("an extension within budget is waited out")
                .status()
                .is_success()
        );
        assert!(
            started.elapsed() >= Duration::from_secs(2),
            "the extended window must be honored, not slept through once: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn deliver_stops_when_window_extension_outlives_budget() {
        // Same mid-wait extension, but the extended window outlives the
        // remaining budget: no request may fire.
        let mock_server = wiremock::MockServer::start().await;

        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_millis(600));

        let extender = limiter.clone();
        let extend = zeroclaw_spawn::spawn!(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            extender.note_rate_limited(30);
        });

        let started = Instant::now();
        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(1),
        )
        .await;
        extend.await.expect("extender task");

        let err = match result {
            Ok(_) => panic!("a window extended past the budget must block the send"),
            Err(err) => err,
        };
        assert!(
            matches!(gate_error(&err), TelegramGateError::BudgetExhausted { .. }),
            "expected BudgetExhausted, got: {err}"
        );
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "the caller waits its whole budget before giving up: {:?}",
            started.elapsed()
        );
        assert!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "no request may fire into the extended window"
        );
    }

    #[tokio::test]
    async fn pre_send_and_retry_waits_share_one_budget() {
        // One 4s operation budget: the pre-send wait consumes ~3s of it, so
        // the post-429 wait sees only ~1s remaining - not a fresh 4s - and
        // the 2s retry window outlives it. The retry must NOT fire.
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let mock_server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 2",
                "parameters": { "retry_after": 2 }
            })))
            .expect(1)
            .named("the initial attempt is the only request")
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_secs(3));

        let started = Instant::now();
        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(4),
        )
        .await;

        let err = match result {
            Ok(_) => panic!("the retry must not get a fresh budget after the pre-send wait"),
            Err(err) => err,
        };
        assert!(
            matches!(gate_error(&err), TelegramGateError::BudgetExhausted { .. }),
            "expected BudgetExhausted, got: {err}"
        );
        assert!(
            started.elapsed() >= Duration::from_secs(4),
            "the operation spends its whole 4s budget across both waits: {:?}",
            started.elapsed()
        );
        assert_eq!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .len(),
            1,
            "the retry never fires once the shared budget is spent"
        );
    }

    #[tokio::test]
    async fn cancelled_deliver_wait_fires_no_request_and_keeps_window() {
        // Dropping the caller mid-wait (turn cancelled) must not disturb the
        // shared window and must not fire a request.
        let mock_server = wiremock::MockServer::start().await;

        let limiter = TelegramRateLimiter::new();
        limiter.deadlines.lock().blocked_until = Some(Instant::now() + Duration::from_secs(30));

        let waiter = limiter.clone();
        let uri = mock_server.uri();
        let task = zeroclaw_spawn::spawn!(async move {
            post_bot_api_json(
                &reqwest::Client::new(),
                &format!("{uri}/bottoken/sendMessage"),
                &serde_json::json!({"chat_id": "1", "text": "hi"}),
                BotApiClass::Deliver,
                &waiter,
                Duration::from_secs(60),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();
        let _ = task.await;

        assert!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "a cancelled waiter must not leave a request in flight"
        );
        assert!(
            matches!(limiter.check(), RateLimitGate::Blocked(_)),
            "the window survives a cancelled waiter for the token's other callers"
        );
    }

    #[tokio::test]
    async fn deliver_retry_after_429_waits_and_sends_once() {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let mock_server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 1",
                "parameters": { "retry_after": 1 }
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true, "result": {"message_id": 1}})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        let started = Instant::now();
        let resp = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(30),
        )
        .await
        .expect("the gated retry delivers");

        assert!(resp.status().is_success());
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "the retry must wait out the noted window: {:?}",
            started.elapsed()
        );
        assert_eq!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .len(),
            2,
            "exactly the flood-limited attempt and one gated retry"
        );
    }

    #[tokio::test]
    async fn second_429_contributes_its_own_longer_deadline() {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        // 1s then 30s: the second 429 must record *its* delay, not re-note
        // the first response's.
        let mock_server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 1",
                "parameters": { "retry_after": 1 }
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 30",
                "parameters": { "retry_after": 30 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendMessage", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "text": "hi"}),
            BotApiClass::Deliver,
            &limiter,
            Duration::from_secs(30),
        )
        .await;

        let err = match result {
            Ok(_) => panic!("a retry answered 429 must not be returned as a usable response"),
            Err(err) => err,
        };
        match gate_error(&err) {
            TelegramGateError::FloodLimited { retry_after_secs } => {
                assert_eq!(*retry_after_secs, 30, "the second 429's own delay");
            }
            other => panic!("expected FloodLimited from the retried 429, got: {other}"),
        }
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(
                    remaining > Duration::from_secs(28),
                    "the shared window must carry the second 429's 30s: {remaining:?}"
                );
            }
            RateLimitGate::Clear => panic!("expected the 30s window to be noted"),
        }
        assert_eq!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .len(),
            2,
            "one attempt, one retry, no fallback fire"
        );
    }

    #[tokio::test]
    async fn droppable_call_makes_no_network_request_inside_window() {
        let mock_server = wiremock::MockServer::start().await;

        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited(30);

        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendChatAction", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "action": "typing"}),
            BotApiClass::Droppable,
            &limiter,
            Duration::from_secs(30),
        )
        .await;

        let err = match result {
            Ok(_) => panic!("a blocked droppable call must be skipped"),
            Err(err) => err,
        };
        assert!(
            matches!(gate_error(&err), TelegramGateError::Skipped { .. }),
            "expected Skipped, got: {err}"
        );
        assert!(
            mock_server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "a skipped call must not reach the network"
        );
    }

    #[tokio::test]
    async fn droppable_429_is_reported_as_flood_limited() {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let mock_server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests: retry after 7",
                "parameters": { "retry_after": 7 }
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let limiter = TelegramRateLimiter::new();
        let result = post_bot_api_json(
            &reqwest::Client::new(),
            &format!("{}/bottoken/sendChatAction", mock_server.uri()),
            &serde_json::json!({"chat_id": "1", "action": "typing"}),
            BotApiClass::Droppable,
            &limiter,
            Duration::from_secs(30),
        )
        .await;

        let err = match result {
            Ok(_) => panic!("a 429-answered droppable call must fail"),
            Err(err) => err,
        };
        match gate_error(&err) {
            TelegramGateError::FloodLimited { retry_after_secs } => {
                assert_eq!(*retry_after_secs, 7);
            }
            other => panic!("expected FloodLimited, got: {other}"),
        }
        assert!(matches!(limiter.check(), RateLimitGate::Blocked(_)));
    }
}
