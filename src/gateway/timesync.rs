//! Cross-device time synchronization and timezone management.
//!
//! MoA uses `occurred_at_utc` as the absolute sort key and
//! `occurred_at_home` (converted to the user's home timezone) as the
//! human-facing display time. This module ensures:
//!
//! 1. **Accurate UTC time** — checked against HTTP Date headers on
//!    startup and periodically (drift detection).
//! 2. **Device timezone detection** — the IANA timezone this device
//!    is currently in (e.g. "America/New_York" when traveling).
//! 3. **Home timezone conversion** — every timestamp is also expressed
//!    in the user's home timezone so that events from different devices
//!    (Korea PC, US phone) appear on a single consistent timeline.
//!
//! ## Multi-device scenario
//!
//! ```text
//! ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
//! │ Home PC      │   │ Office PC    │   │ US Phone     │
//! │ Asia/Seoul   │   │ Asia/Seoul   │   │ America/NY   │
//! │ 2026-03-18   │   │ 2026-03-18   │   │ 2026-03-17   │
//! │ 14:30 KST    │   │ 16:00 KST    │   │ 22:00 EST    │
//! └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
//!        │                  │                  │
//!        ▼                  ▼                  ▼
//!   occurred_at_utc    occurred_at_utc    occurred_at_utc
//!   05:30:00Z          07:00:00Z          03:00:00Z   ← sort key
//!        │                  │                  │
//!        ▼                  ▼                  ▼
//!   occurred_at_home   occurred_at_home   occurred_at_home
//!   14:30 KST          16:00 KST          12:00 KST  ← display
//! ```

use chrono::{DateTime, Utc};
#[cfg(test)]
use chrono::TimeZone;
use chrono_tz::Tz;
use std::sync::RwLock;
use std::time::Duration;

/// Maximum acceptable clock drift before warning the user.
const DRIFT_THRESHOLD: Duration = Duration::from_secs(5);

/// Default interval for periodic clock checks (30 minutes).
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Well-known HTTPS endpoints to check the `Date` header against.
const TIME_CHECK_URLS: &[&str] = &[
    "https://www.google.com",
    "https://www.cloudflare.com",
    "https://www.apple.com",
];

/// Global device time info, updated on each clock check.
static DEVICE_TIME_INFO: RwLock<Option<DeviceTimeInfo>> = RwLock::new(None);

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Snapshot of this device's time state.
#[derive(Debug, Clone)]
pub struct DeviceTimeInfo {
    /// This device's IANA timezone (e.g. "Asia/Seoul", "America/New_York").
    pub device_timezone: String,
    /// The user's home timezone from config.
    pub home_timezone: String,
    /// Last measured clock drift (positive = local is ahead).
    pub drift: Duration,
    /// Whether the drift exceeds the acceptable threshold.
    pub is_drifted: bool,
    /// UTC time of the last successful check.
    pub last_check_utc: DateTime<Utc>,
}

/// Result of a single clock check.
#[derive(Debug, Clone)]
pub struct ClockCheckResult {
    pub local_time: DateTime<Utc>,
    pub remote_time: DateTime<Utc>,
    pub drift: Duration,
    pub is_drifted: bool,
    pub source: String,
}

/// A timestamp expressed in three forms for storage and sync.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TimestampTriple {
    /// ISO-8601 UTC — the absolute sort key for cross-device ordering.
    pub utc: String,
    /// ISO-8601 with the device's local timezone offset.
    pub local: String,
    /// IANA timezone name of the device (e.g. "America/New_York").
    pub device_tz: String,
    /// ISO-8601 converted to the user's home timezone — for display.
    pub home: String,
    /// IANA timezone name of the user's home (e.g. "Asia/Seoul").
    pub home_tz: String,
}

// ---------------------------------------------------------------------------
// Clock drift detection
// ---------------------------------------------------------------------------

/// Perform a single clock check against well-known HTTPS endpoints.
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

        let local_mid = local_before
            + chrono::Duration::milliseconds(
                (local_after - local_before).num_milliseconds() / 2,
            );

        if let Some(date_header) = resp.headers().get("date") {
            if let Ok(date_str) = date_header.to_str() {
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

// ---------------------------------------------------------------------------
// Device timezone detection
// ---------------------------------------------------------------------------

/// Detect the device's current IANA timezone.
///
/// Strategy (in order):
/// 1. `TZ` environment variable (explicit override).
/// 2. `/etc/localtime` symlink target on Linux/macOS.
/// 3. Fall back to UTC.
pub fn detect_device_timezone() -> String {
    // 1. TZ env var
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() && tz.parse::<Tz>().is_ok() {
            return tz;
        }
    }

    // 2. /etc/localtime symlink (Linux/macOS)
    #[cfg(unix)]
    {
        if let Ok(target) = std::fs::read_link("/etc/localtime") {
            let path_str = target.to_string_lossy();
            // Typical: /usr/share/zoneinfo/Asia/Seoul
            if let Some(pos) = path_str.find("zoneinfo/") {
                let tz_name = &path_str[pos + 9..];
                if tz_name.parse::<Tz>().is_ok() {
                    return tz_name.to_string();
                }
            }
        }
    }

    // 3. Fallback
    "UTC".to_string()
}

// ---------------------------------------------------------------------------
// Home timezone conversion
// ---------------------------------------------------------------------------

/// Build a `TimestampTriple` from a UTC instant.
///
/// Converts the UTC time into both the device's local timezone and
/// the user's home timezone, producing three parallel representations
/// of the same instant.
pub fn make_timestamp_triple(
    utc_time: &DateTime<Utc>,
    device_tz_name: &str,
    home_tz_name: &str,
) -> TimestampTriple {
    let device_tz: Tz = device_tz_name.parse().unwrap_or(chrono_tz::UTC);
    let home_tz: Tz = home_tz_name.parse().unwrap_or(chrono_tz::UTC);

    let local_dt = utc_time.with_timezone(&device_tz);
    let home_dt = utc_time.with_timezone(&home_tz);

    TimestampTriple {
        utc: utc_time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        local: local_dt.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
        device_tz: device_tz_name.to_string(),
        home: home_dt.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
        home_tz: home_tz_name.to_string(),
    }
}

/// Convert any ISO-8601 timestamp string to home timezone.
///
/// Useful for normalizing `occurred_at` values from different devices
/// into the user's home timezone for consistent display.
pub fn to_home_timezone(
    iso_timestamp: &str,
    home_tz_name: &str,
) -> Option<String> {
    let home_tz: Tz = home_tz_name.parse().ok()?;

    // Try parsing with timezone offset first
    if let Ok(dt) = DateTime::parse_from_rfc3339(iso_timestamp) {
        let home_dt = dt.with_timezone(&home_tz);
        return Some(home_dt.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string());
    }

    // Try as UTC (no offset)
    if let Ok(dt) = iso_timestamp.parse::<DateTime<Utc>>() {
        let home_dt = dt.with_timezone(&home_tz);
        return Some(home_dt.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string());
    }

    None
}

/// Get the current UTC time as an ISO-8601 string.
pub fn now_utc_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Build a TimestampTriple for "right now" on this device.
pub fn now_triple(home_tz_name: &str) -> TimestampTriple {
    let device_tz = detect_device_timezone();
    make_timestamp_triple(&Utc::now(), &device_tz, home_tz_name)
}

// ---------------------------------------------------------------------------
// Global device time info
// ---------------------------------------------------------------------------

/// Update the global device time info after a successful clock check.
fn update_device_info(result: &ClockCheckResult, home_tz: &str) {
    let info = DeviceTimeInfo {
        device_timezone: detect_device_timezone(),
        home_timezone: home_tz.to_string(),
        drift: result.drift,
        is_drifted: result.is_drifted,
        last_check_utc: result.local_time,
    };
    if let Ok(mut guard) = DEVICE_TIME_INFO.write() {
        *guard = Some(info);
    }
}

/// Get the latest device time info (if a check has been performed).
pub fn get_device_time_info() -> Option<DeviceTimeInfo> {
    DEVICE_TIME_INFO.read().ok().and_then(|g| g.clone())
}

// ---------------------------------------------------------------------------
// Startup and periodic checks
// ---------------------------------------------------------------------------

/// Run a one-shot clock check and log the result.
///
/// Called at gateway startup. Also updates the global DeviceTimeInfo.
pub async fn check_and_log(home_tz: &str) -> Option<Duration> {
    let device_tz = detect_device_timezone();
    tracing::info!(
        device_tz = %device_tz,
        home_tz = %home_tz,
        "Device timezone: {}, Home timezone: {}{}",
        device_tz,
        home_tz,
        if device_tz != home_tz {
            format!(" (cross-timezone: events will be converted to {})", home_tz)
        } else {
            String::new()
        },
    );

    match check_clock_drift().await {
        Some(result) => {
            update_device_info(&result, home_tz);

            if result.is_drifted {
                tracing::warn!(
                    drift_ms = result.drift.as_millis() as u64,
                    local = %result.local_time.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                    remote = %result.remote_time.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                    source = %result.source,
                    "Clock drift detected! Device time is off by {}ms. \
                     This will cause inconsistent timelines across devices. \
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
            // Offline — still record device timezone info with zero drift.
            let info = DeviceTimeInfo {
                device_timezone: device_tz,
                home_timezone: home_tz.to_string(),
                drift: Duration::ZERO,
                is_drifted: false,
                last_check_utc: Utc::now(),
            };
            if let Ok(mut guard) = DEVICE_TIME_INFO.write() {
                *guard = Some(info);
            }
            tracing::debug!(
                "Clock check skipped (offline?), timezone info recorded"
            );
            None
        }
    }
}

/// Spawn a background task that periodically checks clock drift.
pub fn spawn_periodic_check(home_tz: String, interval: Option<Duration>) {
    let interval = interval.unwrap_or(DEFAULT_CHECK_INTERVAL);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip first (startup check done separately)

        loop {
            ticker.tick().await;
            check_and_log(&home_tz).await;
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

    #[test]
    fn make_timestamp_triple_converts_correctly() {
        let utc = Utc.with_ymd_and_hms(2026, 3, 18, 5, 30, 0).unwrap();
        let triple = make_timestamp_triple(&utc, "America/New_York", "Asia/Seoul");

        assert!(triple.utc.contains("05:30:00"));
        assert_eq!(triple.device_tz, "America/New_York");
        assert_eq!(triple.home_tz, "Asia/Seoul");
        // Home (KST = UTC+9): 05:30 + 9 = 14:30
        assert!(triple.home.contains("14:30:00"));
        // Device (EST = UTC-5 in March, actually EDT UTC-4): 01:30 or 00:30
        assert!(triple.local.contains("01:30:00") || triple.local.contains("00:30:00"));
    }

    #[test]
    fn to_home_timezone_works() {
        let utc_str = "2026-03-18T05:30:00.000Z";
        let result = to_home_timezone(utc_str, "Asia/Seoul").unwrap();
        assert!(result.contains("14:30:00"));
    }

    #[test]
    fn to_home_timezone_with_offset() {
        // EST time (UTC-4 in March) → KST
        let est_str = "2026-03-18T01:30:00.000-04:00";
        let result = to_home_timezone(est_str, "Asia/Seoul").unwrap();
        assert!(result.contains("14:30:00"));
    }

    #[test]
    fn detect_timezone_returns_something() {
        let tz = detect_device_timezone();
        assert!(!tz.is_empty());
        // Should be a valid IANA name or "UTC"
        assert!(tz.parse::<Tz>().is_ok());
    }

    #[test]
    fn now_triple_produces_valid_output() {
        let triple = now_triple("Asia/Seoul");
        assert!(triple.utc.ends_with('Z'));
        assert_eq!(triple.home_tz, "Asia/Seoul");
        assert!(!triple.local.is_empty());
        assert!(!triple.home.is_empty());
    }
}
