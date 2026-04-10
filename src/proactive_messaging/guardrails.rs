use super::store;
use super::types::{GuardrailDecision, GuardrailDenialReason, MessagePriority};
use crate::config::{ProactiveMessagingConfig, ProactiveMessagingRateLimits, QuietHoursWindow};
use chrono::{Duration, Timelike, Utc};
use std::path::Path;

/// Evaluate all guardrails for a proactive message send attempt.
///
/// Returns `Allowed` when the message may be delivered immediately, or
/// `Denied(reason)` when it should be queued or rejected.
pub fn evaluate_guardrails(
    config: &ProactiveMessagingConfig,
    workspace_dir: &Path,
    priority: MessagePriority,
) -> GuardrailDecision {
    if !config.enabled {
        return GuardrailDecision::Denied(GuardrailDenialReason::Disabled);
    }

    // Rate limits apply even to urgent messages.
    if let Some(reason) = check_rate_limits(&config.rate_limits, workspace_dir) {
        return GuardrailDecision::Denied(reason);
    }

    // Queue full check.
    if let Ok(pending) = store::count_pending(workspace_dir) {
        if pending >= config.queue.max_pending {
            return GuardrailDecision::Denied(GuardrailDenialReason::QueueFull);
        }
    }

    // Urgent messages bypass quiet hours.
    if priority == MessagePriority::Urgent {
        return GuardrailDecision::Allowed;
    }

    // Quiet hours check.
    if let Some(end_desc) = is_in_quiet_hours(&config.quiet_hours) {
        return GuardrailDecision::Denied(GuardrailDenialReason::QuietHours {
            window_end_description: end_desc,
        });
    }

    GuardrailDecision::Allowed
}

/// Check whether the current time falls within any configured quiet-hours window.
///
/// Returns `Some(description)` with when the window ends if currently in quiet hours,
/// or `None` if outside all windows.
pub fn is_in_quiet_hours(windows: &[QuietHoursWindow]) -> Option<String> {
    for window in windows {
        if is_in_window(window) {
            let desc = format!("{}:00 {}", window.end_hour, window.timezone);
            return Some(desc);
        }
    }
    None
}

/// Test whether the current time falls within a single quiet-hours window.
fn is_in_window(window: &QuietHoursWindow) -> bool {
    let tz: chrono_tz::Tz = match window.timezone.parse() {
        Ok(tz) => tz,
        Err(_) => {
            tracing::warn!(
                "Invalid timezone '{}' in quiet_hours config; skipping window",
                window.timezone
            );
            return false;
        }
    };

    let now_local = Utc::now().with_timezone(&tz);
    let hour = now_local.hour() as u8;

    let start = window.start_hour;
    let end = window.end_hour;

    if start <= end {
        // Simple range, e.g. 1..7
        hour >= start && hour < end
    } else {
        // Wraps past midnight, e.g. 22..7 → 22-23 or 0-6
        hour >= start || hour < end
    }
}

/// Check rate limits against the store's sent-message counts.
fn check_rate_limits(
    limits: &ProactiveMessagingRateLimits,
    workspace_dir: &Path,
) -> Option<GuardrailDenialReason> {
    let now = Utc::now();

    // Hourly limit
    if let Ok(hourly) = store::count_sent_in_window(workspace_dir, now - Duration::hours(1)) {
        if hourly >= limits.max_per_hour {
            return Some(GuardrailDenialReason::RateLimitHourly);
        }
    }

    // Daily limit
    if let Ok(daily) = store::count_sent_in_window(workspace_dir, now - Duration::hours(24)) {
        if daily >= limits.max_per_day {
            return Some(GuardrailDenialReason::RateLimitDaily);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProactiveMessagingQueueConfig, ProactiveMessagingRateLimits};
    use tempfile::TempDir;

    fn test_config() -> ProactiveMessagingConfig {
        ProactiveMessagingConfig {
            enabled: true,
            quiet_hours: Vec::new(),
            rate_limits: ProactiveMessagingRateLimits {
                max_per_hour: 10,
                max_per_day: 50,
            },
            queue: ProactiveMessagingQueueConfig {
                max_pending: 100,
                ttl_hours: 24,
            },
        }
    }

    #[test]
    fn disabled_returns_denied() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = test_config();
        cfg.enabled = false;

        let decision = evaluate_guardrails(&cfg, tmp.path(), MessagePriority::Normal);
        assert!(matches!(
            decision,
            GuardrailDecision::Denied(GuardrailDenialReason::Disabled)
        ));
    }

    #[test]
    fn allowed_when_enabled_and_no_restrictions() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config();

        let decision = evaluate_guardrails(&cfg, tmp.path(), MessagePriority::Normal);
        assert!(matches!(decision, GuardrailDecision::Allowed));
    }

    #[test]
    fn quiet_hours_simple_range() {
        // Window from hour 1 to hour 23 (almost always active)
        let window = QuietHoursWindow {
            start_hour: 0,
            end_hour: 23,
            timezone: "UTC".to_string(),
        };
        // Current UTC hour is almost certainly in 0..23
        let result = is_in_window(&window);
        // This will be true unless test runs at exactly 23:xx UTC
        assert!(result || Utc::now().hour() >= 23);
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let window = QuietHoursWindow {
            start_hour: 22,
            end_hour: 7,
            timezone: "UTC".to_string(),
        };
        let hour = Utc::now().hour() as u8;
        let expected = hour >= 22 || hour < 7;
        assert_eq!(is_in_window(&window), expected);
    }

    #[test]
    fn invalid_timezone_skips_window() {
        let window = QuietHoursWindow {
            start_hour: 0,
            end_hour: 23,
            timezone: "Invalid/Timezone".to_string(),
        };
        assert!(!is_in_window(&window));
    }

    #[test]
    fn urgent_bypasses_quiet_hours() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = test_config();
        cfg.quiet_hours = vec![QuietHoursWindow {
            start_hour: 0,
            end_hour: 23,
            timezone: "UTC".to_string(),
        }];

        let decision = evaluate_guardrails(&cfg, tmp.path(), MessagePriority::Urgent);
        assert!(matches!(decision, GuardrailDecision::Allowed));
    }

    #[test]
    fn rate_limit_hourly() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = test_config();
        cfg.rate_limits.max_per_hour = 2;

        // Enqueue and mark sent to simulate rate limit
        for _ in 0..2 {
            let msg = store::enqueue_message(
                tmp.path(),
                "sig",
                "+1",
                "test",
                MessagePriority::Normal,
                None,
                None,
                24,
            )
            .unwrap();
            store::mark_sent(tmp.path(), &[&msg.id]).unwrap();
        }

        let decision = evaluate_guardrails(&cfg, tmp.path(), MessagePriority::Normal);
        assert!(matches!(
            decision,
            GuardrailDecision::Denied(GuardrailDenialReason::RateLimitHourly)
        ));
    }

    #[test]
    fn queue_full_check() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = test_config();
        cfg.queue.max_pending = 2;

        for i in 0..2 {
            store::enqueue_message(
                tmp.path(),
                "sig",
                "+1",
                &format!("msg {i}"),
                MessagePriority::Normal,
                None,
                None,
                24,
            )
            .unwrap();
        }

        let decision = evaluate_guardrails(&cfg, tmp.path(), MessagePriority::Normal);
        assert!(matches!(
            decision,
            GuardrailDecision::Denied(GuardrailDenialReason::QueueFull)
        ));
    }
}
