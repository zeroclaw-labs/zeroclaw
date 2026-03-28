use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Configuration for calendar-driven trigger polling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarTriggerConfig {
    /// Calendar provider (e.g. "microsoft365")
    pub calendar_source: String,
    /// Seconds between polls for upcoming events (default 300)
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Minutes after event start before declaring no-show (default 10)
    #[serde(default = "default_no_show_threshold")]
    pub no_show_threshold_minutes: u32,
    /// Calendar IDs to watch (empty = primary calendar only)
    #[serde(default)]
    pub watch_calendars: Vec<String>,
}

fn default_poll_interval() -> u64 {
    300
}
fn default_no_show_threshold() -> u32 {
    10
}

/// Emitted when a calendar event's expected attendee doesn't show.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoShowEvent {
    pub event_id: String,
    pub event_title: String,
    pub expected_start: DateTime<Utc>,
    pub detected_at: DateTime<Utc>,
    pub calendar_source: String,
    /// Which calendar this event was in
    #[serde(default)]
    pub calendar_id: String,
}

/// A tracked upcoming calendar event being monitored for no-shows.
#[derive(Debug, Clone)]
pub struct TrackedEvent {
    pub event_id: String,
    pub event_title: String,
    pub start_time: DateTime<Utc>,
    pub calendar_id: String,
}
