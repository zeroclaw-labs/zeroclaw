use chrono::{Duration, Utc};
use zeroclaw::calendar::poller::CalendarPoller;
use zeroclaw::calendar::types::{CalendarTriggerConfig, NoShowEvent, TrackedEvent};
use zeroclaw::sop::types::{SopTrigger, SopTriggerSource};

fn default_config() -> CalendarTriggerConfig {
    CalendarTriggerConfig {
        calendar_source: "microsoft365".to_string(),
        poll_interval_secs: 300,
        no_show_threshold_minutes: 10,
        watch_calendars: vec![],
    }
}

#[test]
fn calendar_trigger_config_defaults() {
    let json = r#"{"calendar_source": "microsoft365"}"#;
    let config: CalendarTriggerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.poll_interval_secs, 300);
    assert_eq!(config.no_show_threshold_minutes, 10);
    assert!(config.watch_calendars.is_empty());
}

#[test]
fn calendar_trigger_config_custom_values() {
    let json = r#"{"calendar_source":"google","poll_interval_secs":60,"no_show_threshold_minutes":5,"watch_calendars":["cal1","cal2"]}"#;
    let config: CalendarTriggerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.calendar_source, "google");
    assert_eq!(config.poll_interval_secs, 60);
    assert_eq!(config.no_show_threshold_minutes, 5);
    assert_eq!(config.watch_calendars, vec!["cal1", "cal2"]);
}

#[test]
fn no_show_event_serialization_roundtrip() {
    let now = Utc::now();
    let event = NoShowEvent {
        event_id: "evt-123".to_string(),
        event_title: "Team standup".to_string(),
        expected_start: now,
        detected_at: now + Duration::minutes(10),
        calendar_source: "microsoft365".to_string(),
        calendar_id: "primary".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let deser: NoShowEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event, deser);
}

#[test]
fn poller_track_events() {
    let mut poller = CalendarPoller::new(default_config());
    let events = vec![
        TrackedEvent {
            event_id: "evt-1".to_string(),
            event_title: "Meeting A".to_string(),
            start_time: Utc::now() + Duration::minutes(30),
            calendar_id: "primary".to_string(),
        },
        TrackedEvent {
            event_id: "evt-2".to_string(),
            event_title: "Meeting B".to_string(),
            start_time: Utc::now() + Duration::hours(1),
            calendar_id: "primary".to_string(),
        },
    ];
    let added = poller.track_events(events);
    assert_eq!(added, 2);
    assert_eq!(poller.tracked_count(), 2);
}

#[test]
fn poller_detect_no_show_after_threshold() {
    let mut poller = CalendarPoller::new(default_config());
    let start = Utc::now() - Duration::minutes(15); // 15 min ago
    poller.track_events(vec![TrackedEvent {
        event_id: "evt-1".to_string(),
        event_title: "Standup".to_string(),
        start_time: start,
        calendar_id: "primary".to_string(),
    }]);

    let events = poller.detect_no_shows(Utc::now());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, SopTriggerSource::Calendar);
    assert_eq!(events[0].topic.as_deref(), Some("calendar.no_show"));

    // Parse payload
    let payload: NoShowEvent = serde_json::from_str(events[0].payload.as_ref().unwrap()).unwrap();
    assert_eq!(payload.event_id, "evt-1");
    assert_eq!(payload.event_title, "Standup");
    assert_eq!(payload.calendar_source, "microsoft365");
}

#[test]
fn poller_no_detection_before_threshold() {
    let mut poller = CalendarPoller::new(default_config());
    let start = Utc::now() - Duration::minutes(5); // Only 5 min ago, threshold is 10
    poller.track_events(vec![TrackedEvent {
        event_id: "evt-1".to_string(),
        event_title: "Standup".to_string(),
        start_time: start,
        calendar_id: "primary".to_string(),
    }]);

    let events = poller.detect_no_shows(Utc::now());
    assert_eq!(events.len(), 0);
}

#[test]
fn poller_no_duplicate_fires() {
    let mut poller = CalendarPoller::new(default_config());
    let start = Utc::now() - Duration::minutes(15);
    poller.track_events(vec![TrackedEvent {
        event_id: "evt-1".to_string(),
        event_title: "Standup".to_string(),
        start_time: start,
        calendar_id: "primary".to_string(),
    }]);

    let events1 = poller.detect_no_shows(Utc::now());
    assert_eq!(events1.len(), 1);

    // Second detection should not re-fire
    let events2 = poller.detect_no_shows(Utc::now());
    assert_eq!(events2.len(), 0);
    assert_eq!(poller.fired_count(), 1);
}

#[test]
fn poller_already_fired_events_not_retracked() {
    let mut poller = CalendarPoller::new(default_config());
    let start = Utc::now() - Duration::minutes(15);
    let tracked = vec![TrackedEvent {
        event_id: "evt-1".to_string(),
        event_title: "Standup".to_string(),
        start_time: start,
        calendar_id: "primary".to_string(),
    }];

    poller.track_events(tracked.clone());
    poller.detect_no_shows(Utc::now()); // fires evt-1

    // Try to re-add the same event
    let added = poller.track_events(tracked);
    assert_eq!(added, 0); // Should not be re-tracked
}

#[test]
fn poller_cleanup_stale_events() {
    let mut poller = CalendarPoller::new(default_config());
    let old_start = Utc::now() - Duration::hours(2); // 2 hours ago — stale
    let recent_start = Utc::now() + Duration::minutes(30); // upcoming

    poller.track_events(vec![
        TrackedEvent {
            event_id: "old".to_string(),
            event_title: "Old meeting".to_string(),
            start_time: old_start,
            calendar_id: "primary".to_string(),
        },
        TrackedEvent {
            event_id: "upcoming".to_string(),
            event_title: "Future meeting".to_string(),
            start_time: recent_start,
            calendar_id: "primary".to_string(),
        },
    ]);

    assert_eq!(poller.tracked_count(), 2);
    poller.cleanup_stale(Utc::now());
    assert_eq!(poller.tracked_count(), 1); // only upcoming remains
}

#[test]
fn sop_trigger_calendar_variant_display() {
    let trigger = SopTrigger::Calendar {
        calendar_source: "microsoft365".to_string(),
        calendar_ids: vec!["primary".to_string()],
    };
    assert_eq!(trigger.to_string(), "calendar:microsoft365");
}

#[test]
fn sop_trigger_source_calendar_display() {
    assert_eq!(SopTriggerSource::Calendar.to_string(), "calendar");
}

#[test]
fn sop_trigger_calendar_serialization() {
    let trigger = SopTrigger::Calendar {
        calendar_source: "microsoft365".to_string(),
        calendar_ids: vec![],
    };
    let json = serde_json::to_string(&trigger).unwrap();
    let deser: SopTrigger = serde_json::from_str(&json).unwrap();
    assert_eq!(trigger, deser);
}

#[test]
fn sop_trigger_source_calendar_serialization() {
    let source = SopTriggerSource::Calendar;
    let json = serde_json::to_string(&source).unwrap();
    assert_eq!(json, "\"calendar\"");
    let deser: SopTriggerSource = serde_json::from_str(&json).unwrap();
    assert_eq!(source, deser);
}
