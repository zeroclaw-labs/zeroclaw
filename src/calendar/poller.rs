use crate::calendar::types::{CalendarTriggerConfig, NoShowEvent, TrackedEvent};
use crate::sop::types::{SopEvent, SopTriggerSource};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};

pub struct CalendarPoller {
    config: CalendarTriggerConfig,
    /// Events currently being tracked: event_id -> TrackedEvent
    tracked_events: HashMap<String, TrackedEvent>,
    /// Event IDs that already fired a no-show (prevent duplicates)
    already_fired: HashSet<String>,
}

impl CalendarPoller {
    pub fn new(config: CalendarTriggerConfig) -> Self {
        Self {
            config,
            tracked_events: HashMap::new(),
            already_fired: HashSet::new(),
        }
    }

    /// Update the set of tracked events from a calendar query result.
    /// Returns how many events were added/updated.
    pub fn track_events(&mut self, events: Vec<TrackedEvent>) -> usize {
        let mut count = 0;
        for event in events {
            if !self.already_fired.contains(&event.event_id) {
                self.tracked_events.insert(event.event_id.clone(), event);
                count += 1;
            }
        }
        count
    }

    /// Detect no-shows: events whose start_time + threshold has passed.
    /// Returns SopEvents for each detected no-show.
    pub fn detect_no_shows(&mut self, now: DateTime<Utc>) -> Vec<SopEvent> {
        let threshold = Duration::minutes(i64::from(self.config.no_show_threshold_minutes));
        let mut sop_events = Vec::new();
        let mut fired_ids = Vec::new();

        for (id, event) in &self.tracked_events {
            if self.already_fired.contains(id) {
                continue;
            }
            if now >= event.start_time + threshold {
                let no_show = NoShowEvent {
                    event_id: event.event_id.clone(),
                    event_title: event.event_title.clone(),
                    expected_start: event.start_time,
                    detected_at: now,
                    calendar_source: self.config.calendar_source.clone(),
                    calendar_id: event.calendar_id.clone(),
                };
                let sop_event = SopEvent {
                    source: SopTriggerSource::Calendar,
                    topic: Some("calendar.no_show".to_string()),
                    payload: Some(serde_json::to_string(&no_show).unwrap()),
                    timestamp: now.to_rfc3339(),
                };
                sop_events.push(sop_event);
                fired_ids.push(id.clone());
            }
        }

        for id in fired_ids {
            self.already_fired.insert(id.clone());
            self.tracked_events.remove(&id);
        }

        sop_events
    }

    /// Remove tracked events that are too old (past + 1 hour). Prevents memory leak.
    pub fn cleanup_stale(&mut self, now: DateTime<Utc>) {
        let cutoff = now - Duration::hours(1);
        self.tracked_events.retain(|_, e| e.start_time > cutoff);
        // Also clean already_fired for very old events (> 24h)
        // We'd need timestamps for that, but for now we clean tracked_events
    }

    /// Number of currently tracked events.
    pub fn tracked_count(&self) -> usize {
        self.tracked_events.len()
    }

    /// Number of events that have already fired no-show alerts.
    pub fn fired_count(&self) -> usize {
        self.already_fired.len()
    }

    /// Access config (for trigger matching)
    pub fn config(&self) -> &CalendarTriggerConfig {
        &self.config
    }
}
