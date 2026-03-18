//! Lightweight clock-drift detection for cross-device time consistency.
//!
//! MoA uses `occurred_at` (real-world time) as the primary sort key for
//! ontology actions and sync deltas. If a device's clock is significantly
//! wrong, the entire timeline becomes inconsistent across devices.
//!
//! This module provides:
//! - One-shot clock check on gateway startup.
//! - Periodic background check (configurable interval, default 30 min).
//! - Warning log + optional user notification when drift exceeds threshold.
//!
//! Implementation: sends a HEAD request to a well-known HTTPS endpoint and
//! compares the `Date` response header against the local clock. No NTP
//! dependency required — HTTP Date headers are accurate to ~1 second.

use chrono::{DateTime, Utc};
use std::time::Duration;

/// Maximum acceptable clock drift before warning the user.
const DRIFT_THRESHOLD: Duration = Duration::from_secs(5);

/// Default interval for periodic clock checks (30 minutes).
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Well-known HTTPS endpoints to check the `Date` header against.
/// We try them in order; first successful response wins.
const TIME_CHECK_URLS: &[&str] = &[
    "https://www.google.com",
    "https://www.cloudflare.com",
    "https://www.apple.com",
];

/// Result of a single clock check.
#[derive(Debug, Clone)]
pub struct ClockCheckResult {
    /// Local time at the moment of the check.
    pub local_time: DateTime<Utc>,
    /// Remote time from the HTTP Date header.
    pub remote_time: DateTime<Utc>,
    /// Absolute drift (always positive).
    pub drift: Duration,
    /// Whether the drift exceeds the acceptable threshold.
    pub is_drifted: bool,
    /// Which endpoint responded.
    pub source: String,
}

/// Perform a single clock check against well-known HTTPS endpoints.
///
/// Returns `None` if all endpoints are unreachable (e.g. offline mode).
pub async fn check_clock_drift() -> Option<ClockCheckResult> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;

    for url in TIME_CHECK_URLS {
        let local_before = Utc::now();
        let resp = match client.head(*url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let local_after = Utc::now();

        // Use the midpoint of request/response as local reference
        let local_mid = local_before
            + chrono::Duration::milliseconds(
                (local_after - local_before).num_milliseconds() / 2,
            );

        if let Some(date_header) = resp.headers().get("date") {
            if let Ok(date_str) = date_header.to_str() {
                // HTTP Date format: "Thu, 18 Mar 2026 05:30:00 GMT"
                if let Ok(remote_time) = DateTime::parse_from_rfc2822(date_str) {
                    let remote_utc = remote_time.with_timezone(&Utc);
                    let diff = (local_mid - remote_utc).abs();
                    let drift = diff.to_std().unwrap_or(Duration::ZERO);

                    return Some(ClockCheckResult {
                        local_time: local_mid,
                        remote_time: remote_utc,
                        drift,
                        is_drifted: drift > DRIFT_THRESHOLD,
                        source: url.to_string(),
                    });
                }
            }
        }
    }

    None
}

/// Run a one-shot clock check and log the result.
///
/// Called at gateway startup. Returns the drift duration if a check
/// succeeded, `None` if all endpoints were unreachable.
pub async fn check_and_log() -> Option<Duration> {
    match check_clock_drift().await {
        Some(result) => {
            if result.is_drifted {
                tracing::warn!(
                    drift_ms = result.drift.as_millis() as u64,
                    local = %result.local_time.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                    remote = %result.remote_time.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                    source = %result.source,
                    "Clock drift detected! Device time is off by {}ms. \
                     This may cause inconsistent timelines across devices. \
                     Please synchronize your system clock.",
                    result.drift.as_millis(),
                );
            } else {
                tracing::info!(
                    drift_ms = result.drift.as_millis() as u64,
                    source = %result.source,
                    "Clock check OK (drift: {}ms, threshold: {}ms)",
                    result.drift.as_millis(),
                    DRIFT_THRESHOLD.as_millis(),
                );
            }
            Some(result.drift)
        }
        None => {
            tracing::debug!(
                "Clock check skipped — no time endpoints reachable (offline mode?)"
            );
            None
        }
    }
}

/// Spawn a background task that periodically checks clock drift.
///
/// The task runs indefinitely until the runtime shuts down. It checks
/// once every `interval` (default: 30 minutes) and logs warnings when
/// drift exceeds the threshold.
pub fn spawn_periodic_check(interval: Option<Duration>) {
    let interval = interval.unwrap_or(DEFAULT_CHECK_INTERVAL);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the first tick (startup check is done separately).
        ticker.tick().await;

        loop {
            ticker.tick().await;
            check_and_log().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_threshold_is_reasonable() {
        assert!(DRIFT_THRESHOLD.as_secs() <= 30);
        assert!(DRIFT_THRESHOLD.as_secs() >= 1);
    }

    #[test]
    fn default_check_interval_is_reasonable() {
        assert!(DEFAULT_CHECK_INTERVAL.as_secs() >= 60);
        assert!(DEFAULT_CHECK_INTERVAL.as_secs() <= 7200);
    }
}
