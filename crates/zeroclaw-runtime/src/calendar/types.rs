use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// SOP topic emitted for calendar no-show detections.
pub const CALENDAR_NO_SHOW_TOPIC: &str = "calendar.no_show";

/// Configuration for source-agnostic calendar no-show trigger polling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarTriggerConfig {
    /// Calendar provider identifier, for example `microsoft365` or `google`.
    pub calendar_source: String,
    /// Seconds between upstream calendar polls.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Minutes after event start before declaring a no-show.
    #[serde(default = "default_no_show_threshold_minutes")]
    pub no_show_threshold_minutes: u32,
    /// Calendar IDs to watch. Empty means the provider's default calendar set.
    #[serde(default)]
    pub watch_calendars: Vec<String>,
}

fn default_poll_interval_secs() -> u64 {
    300
}

fn default_no_show_threshold_minutes() -> u32 {
    10
}

/// Emitted when a tracked calendar event reaches its no-show threshold.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarNoShowEvent {
    pub event_id: String,
    pub event_title: String,
    pub expected_start: DateTime<Utc>,
    pub detected_at: DateTime<Utc>,
    pub calendar_source: String,
    #[serde(default)]
    pub calendar_id: String,
}

/// A provider-supplied upcoming event being monitored for no-shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedCalendarEvent {
    pub event_id: String,
    pub event_title: String,
    pub start_time: DateTime<Utc>,
    pub calendar_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn calendar_trigger_config_defaults() {
        let config: CalendarTriggerConfig =
            serde_json::from_str(r#"{"calendar_source":"microsoft365"}"#).unwrap();

        assert_eq!(config.calendar_source, "microsoft365");
        assert_eq!(config.poll_interval_secs, 300);
        assert_eq!(config.no_show_threshold_minutes, 10);
        assert!(config.watch_calendars.is_empty());
    }

    #[test]
    fn calendar_trigger_config_custom_values() {
        let config: CalendarTriggerConfig = serde_json::from_str(
            r#"{"calendar_source":"google","poll_interval_secs":60,"no_show_threshold_minutes":5,"watch_calendars":["primary","team"]}"#,
        )
        .unwrap();

        assert_eq!(config.calendar_source, "google");
        assert_eq!(config.poll_interval_secs, 60);
        assert_eq!(config.no_show_threshold_minutes, 5);
        assert_eq!(config.watch_calendars, vec!["primary", "team"]);
    }

    #[test]
    fn calendar_no_show_event_roundtrips() {
        let now = Utc::now();
        let event = CalendarNoShowEvent {
            event_id: "evt-123".to_string(),
            event_title: "Team standup".to_string(),
            expected_start: now,
            detected_at: now + Duration::minutes(10),
            calendar_source: "microsoft365".to_string(),
            calendar_id: "primary".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let roundtrip: CalendarNoShowEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(event, roundtrip);
    }
}
