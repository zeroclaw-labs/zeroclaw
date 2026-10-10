//! Per-token Telegram Bot API flood-control state.
//!
//! Telegram answers excess requests with HTTP 429 and a `retry_after` hint
//! (`parameters.retry_after` in the JSON envelope, or `retry after <N>` in
//! the human-readable description). Firing again inside that window escalates
//! the token's penalty, so this module keeps one shared "blocked until"
//! instant per channel instance and routes every Bot API POST through it.
//!
//! State-source justification (AGENTS.md): `blocked_until` is newly created
//! runtime state — the only local mirror of Telegram's own server-side
//! limiter. It duplicates no config key and no other runtime field.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Fallback window when a 429 body carries no parseable `retry_after`.
/// Telegram's Bot API contract always sends one, so this only covers
/// proxy-generated or truncated answers.
const FALLBACK_RETRY_AFTER_SECS: u64 = 5;

/// Ceiling for one recorded flood-control window. Keeps the `Instant`
/// arithmetic infallible and bounds how long a bogus `retry_after` (from a
/// misbehaving self-hosted Bot API server) can silence the token.
const MAX_BLOCK_WINDOW_SECS: u64 = 3600;

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

/// Why a gated Bot API call produced no usable response. Both variants mean
/// "nothing was delivered and the shared window already accounts for it";
/// Droppable-class callers log at DEBUG and move on.
#[derive(Debug)]
pub(crate) enum TelegramGateError {
    /// Pre-send gate: the token is inside a flood-control window, so a
    /// Droppable call was skipped without any network call.
    Skipped { remaining: Duration },
    /// The call reached Telegram and was answered 429; the window was noted
    /// and the body consumed to harvest `retry_after`.
    FloodLimited { retry_after_secs: u64 },
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
        }
    }
}

impl std::error::Error for TelegramGateError {}

/// Shared per-token flood-control window. `Clone` handles (channel fields,
/// spawned typing loops) all point at the same instant, so a 429 observed by
/// any call blocks every later call on the token, whatever chat it targets.
#[derive(Debug, Clone, Default)]
pub(crate) struct TelegramRateLimiter {
    blocked_until: Arc<Mutex<Option<Instant>>>,
}

impl TelegramRateLimiter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Block the token for `retry_after_secs` plus up to 2s of jitter, so
    /// several chats blocked by the same 429 do not all resume on the same
    /// tick. Jitter derives from the wall clock's sub-second nanos; no `rand`
    /// dependency is warranted for a 3-way pick.
    pub(crate) fn note_rate_limited(&self, retry_after_secs: u64) {
        let jitter_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()) % 3)
            .unwrap_or(0);
        let window_secs = retry_after_secs
            .min(MAX_BLOCK_WINDOW_SECS)
            .saturating_add(jitter_secs);
        *self.blocked_until.lock() = Some(Instant::now() + Duration::from_secs(window_secs));
    }

    /// Inspect the window, clearing it when it has expired.
    pub(crate) fn check(&self) -> RateLimitGate {
        let mut guard = self.blocked_until.lock();
        match *guard {
            Some(until) => {
                let now = Instant::now();
                if until > now {
                    RateLimitGate::Blocked(until.duration_since(now))
                } else {
                    *guard = None;
                    RateLimitGate::Clear
                }
            }
            None => RateLimitGate::Clear,
        }
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

/// POST `body` to a Bot API `url` through the shared flood-control gate.
///
/// - Pre-send, a Droppable call inside the window fails with
///   [`TelegramGateError::Skipped`] without any network call; a Deliver call
///   waits out the window (bounded by `max_wait`) first.
/// - On a 429 answer the window is noted from `retry_after`. Droppable calls
///   then fail with [`TelegramGateError::FloodLimited`] (the body was consumed
///   for parsing, so no response survives to return); Deliver calls wait
///   `min(retry_after, max_wait)` and retry once. A retry that also 429s
///   renews the note and is returned as-is for the caller's existing
///   non-success handling.
///
/// Any other status — success or failure — is returned untouched, and
/// transport errors propagate.
pub(crate) async fn post_bot_api_json(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    class: BotApiClass,
    limiter: &TelegramRateLimiter,
    max_wait: Duration,
) -> anyhow::Result<reqwest::Response> {
    match limiter.check() {
        RateLimitGate::Clear => {}
        RateLimitGate::Blocked(remaining) => match class {
            BotApiClass::Droppable => {
                return Err(TelegramGateError::Skipped { remaining }.into());
            }
            BotApiClass::Deliver => {
                tokio::time::sleep(remaining.min(max_wait)).await;
            }
        },
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
            tokio::time::sleep(Duration::from_secs(retry_after_secs).min(max_wait)).await;
            let retry = client.post(url).json(body).send().await?;
            if retry.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                limiter.note_rate_limited(retry_after_secs);
            }
            Ok(retry)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        *limiter.blocked_until.lock() = Some(Instant::now() + Duration::from_millis(40));

        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(remaining <= Duration::from_millis(40))
            }
            RateLimitGate::Clear => panic!("expected blocked while window is live"),
        }

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(limiter.check(), RateLimitGate::Clear);
        assert!(
            limiter.blocked_until.lock().is_none(),
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
    fn note_rate_limited_caps_absurd_windows() {
        let limiter = TelegramRateLimiter::new();
        limiter.note_rate_limited(u64::MAX);
        match limiter.check() {
            RateLimitGate::Blocked(remaining) => {
                assert!(remaining <= Duration::from_secs(MAX_BLOCK_WINDOW_SECS + 2));
            }
            RateLimitGate::Clear => panic!("expected blocked after note_rate_limited"),
        }
    }
}
