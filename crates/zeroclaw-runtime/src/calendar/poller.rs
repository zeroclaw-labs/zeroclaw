use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::calendar::types::{
    CALENDAR_NO_SHOW_TOPIC, CalendarNoShowEvent, CalendarTriggerConfig, TrackedCalendarEvent,
};
use crate::sop::types::{SopEvent, SopTriggerSource};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CalendarEventKey {
    event_id: String,
    calendar_id: String,
    start_time: DateTime<Utc>,
}

impl CalendarEventKey {
    fn from_event(event: &TrackedCalendarEvent) -> Self {
        Self {
            event_id: event.event_id.clone(),
            calendar_id: event.calendar_id.clone(),
            start_time: event.start_time,
        }
    }
}

/// Source-agnostic in-memory no-show detector for upstream calendar events.
pub struct CalendarPoller {
    config: CalendarTriggerConfig,
    tracked_events: HashMap<CalendarEventKey, TrackedCalendarEvent>,
    already_fired: HashMap<CalendarEventKey, DateTime<Utc>>,
}

impl CalendarPoller {
    pub fn new(config: CalendarTriggerConfig) -> Self {
        Self {
            config,
            tracked_events: HashMap::new(),
            already_fired: HashMap::new(),
        }
    }

    /// Update the tracked event set from a provider query result.
    pub fn track_events(
        &mut self,
        events: impl IntoIterator<Item = TrackedCalendarEvent>,
    ) -> usize {
        let mut tracked = 0usize;
        for event in events {
            let key = CalendarEventKey::from_event(&event);
            if self.already_fired.contains_key(&key) {
                continue;
            }
            self.tracked_events.insert(key, event);
            tracked += 1;
        }
        tracked
    }

    /// Emit one SOP event for each tracked event past its no-show threshold.
    pub fn detect_no_shows(&mut self, now: DateTime<Utc>) -> Vec<SopEvent> {
        let threshold = Duration::minutes(i64::from(self.config.no_show_threshold_minutes));
        let mut sop_events = Vec::new();
        let mut fired_ids = Vec::new();

        for (key, event) in &self.tracked_events {
            if self.already_fired.contains_key(key) || now < event.start_time + threshold {
                continue;
            }

            let no_show = CalendarNoShowEvent {
                event_id: key.event_id.clone(),
                event_title: event.event_title.clone(),
                expected_start: event.start_time,
                detected_at: now,
                calendar_source: self.config.calendar_source.clone(),
                calendar_id: event.calendar_id.clone(),
            };

            if let Ok(payload) = serde_json::to_string(&no_show) {
                sop_events.push(SopEvent {
                    source: SopTriggerSource::Calendar,
                    topic: Some(CALENDAR_NO_SHOW_TOPIC.to_string()),
                    payload: Some(payload),
                    timestamp: now.to_rfc3339(),
                });
                fired_ids.push(key.clone());
            }
        }

        for key in fired_ids {
            self.already_fired.insert(key.clone(), now);
            self.tracked_events.remove(&key);
        }

        sop_events
    }

    /// Remove tracked events that are too old to fire a useful no-show.
    pub fn cleanup_stale(&mut self, now: DateTime<Utc>) {
        let retention = self.retention_window();
        self.tracked_events
            .retain(|key, _| key.start_time + retention > now);
        self.already_fired
            .retain(|key, _| key.start_time + retention > now);
    }

    pub fn tracked_count(&self) -> usize {
        self.tracked_events.len()
    }

    pub fn fired_count(&self) -> usize {
        self.already_fired.len()
    }

    pub fn config(&self) -> &CalendarTriggerConfig {
        &self.config
    }

    fn retention_window(&self) -> Duration {
        Duration::minutes(i64::from(self.config.no_show_threshold_minutes)) + Duration::hours(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> CalendarTriggerConfig {
        CalendarTriggerConfig {
            calendar_source: "microsoft365".to_string(),
            poll_interval_secs: 300,
            no_show_threshold_minutes: 10,
            watch_calendars: Vec::new(),
        }
    }

    fn tracked_event(event_id: &str, start_time: DateTime<Utc>) -> TrackedCalendarEvent {
        TrackedCalendarEvent {
            event_id: event_id.to_string(),
            event_title: format!("Meeting {event_id}"),
            start_time,
            calendar_id: "primary".to_string(),
        }
    }

    fn tracked_event_for_calendar(
        event_id: &str,
        calendar_id: &str,
        start_time: DateTime<Utc>,
    ) -> TrackedCalendarEvent {
        TrackedCalendarEvent {
            event_id: event_id.to_string(),
            event_title: format!("Meeting {event_id}"),
            start_time,
            calendar_id: calendar_id.to_string(),
        }
    }

    #[test]
    fn tracks_provider_events() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());

        let added = poller.track_events([
            tracked_event("evt-1", now + Duration::minutes(30)),
            tracked_event("evt-2", now + Duration::hours(1)),
        ]);

        assert_eq!(added, 2);
        assert_eq!(poller.tracked_count(), 2);
        assert_eq!(poller.fired_count(), 0);
    }

    #[test]
    fn emits_no_show_after_threshold() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        poller.track_events([tracked_event("evt-1", now - Duration::minutes(15))]);

        let events = poller.detect_no_shows(now);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, SopTriggerSource::Calendar);
        assert_eq!(events[0].topic.as_deref(), Some(CALENDAR_NO_SHOW_TOPIC));

        let payload: CalendarNoShowEvent =
            serde_json::from_str(events[0].payload.as_deref().unwrap()).unwrap();
        assert_eq!(payload.event_id, "evt-1");
        assert_eq!(payload.event_title, "Meeting evt-1");
        assert_eq!(payload.calendar_source, "microsoft365");
        assert_eq!(payload.calendar_id, "primary");
    }

    #[test]
    fn does_not_emit_before_threshold() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        poller.track_events([tracked_event("evt-1", now - Duration::minutes(5))]);

        assert!(poller.detect_no_shows(now).is_empty());
        assert_eq!(poller.tracked_count(), 1);
    }

    #[test]
    fn does_not_fire_duplicates_or_retrack_fired_events() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        let tracked = tracked_event("evt-1", now - Duration::minutes(15));

        assert_eq!(poller.track_events([tracked.clone()]), 1);
        assert_eq!(poller.detect_no_shows(now).len(), 1);
        assert!(poller.detect_no_shows(now).is_empty());
        assert_eq!(poller.fired_count(), 1);
        assert_eq!(poller.track_events([tracked]), 0);
    }

    #[test]
    fn separates_matching_event_ids_across_calendars() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        poller.track_events([
            tracked_event_for_calendar("evt-1", "primary", now - Duration::minutes(15)),
            tracked_event_for_calendar("evt-1", "team", now - Duration::minutes(15)),
        ]);

        let events = poller.detect_no_shows(now);
        let mut calendar_ids: Vec<_> = events
            .iter()
            .map(|event| {
                let payload: CalendarNoShowEvent =
                    serde_json::from_str(event.payload.as_deref().unwrap()).unwrap();
                payload.calendar_id
            })
            .collect();
        calendar_ids.sort();

        assert_eq!(events.len(), 2);
        assert_eq!(calendar_ids, vec!["primary", "team"]);
    }

    #[test]
    fn cleanup_stale_keeps_future_events() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        poller.track_events([
            tracked_event("old", now - Duration::hours(2)),
            tracked_event("upcoming", now + Duration::minutes(30)),
        ]);

        poller.cleanup_stale(now);

        assert_eq!(poller.tracked_count(), 1);
    }

    #[test]
    fn cleanup_stale_respects_long_no_show_thresholds() {
        let now = Utc::now();
        let mut config = default_config();
        config.no_show_threshold_minutes = 90;
        let mut poller = CalendarPoller::new(config);
        poller.track_events([tracked_event("evt-1", now - Duration::minutes(61))]);

        poller.cleanup_stale(now);

        assert_eq!(poller.tracked_count(), 1);
        assert!(poller.detect_no_shows(now).is_empty());
    }

    #[test]
    fn cleanup_stale_bounds_fired_state() {
        let now = Utc::now();
        let mut poller = CalendarPoller::new(default_config());
        poller.track_events([tracked_event("evt-1", now - Duration::minutes(15))]);
        assert_eq!(poller.detect_no_shows(now).len(), 1);
        assert_eq!(poller.fired_count(), 1);

        poller.cleanup_stale(now + Duration::hours(2));

        assert_eq!(poller.fired_count(), 0);
    }
}
