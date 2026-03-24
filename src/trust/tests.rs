use super::*;
use chrono::{Duration, Utc};

// ── TrustConfig Tests ──────────────────────────────────────────

#[test]
fn trust_config_defaults() {
    let config = TrustConfig::default();
    assert_eq!(config.initial_score, 0.8);
    assert_eq!(config.decay_half_life_days, 30.0);
    assert_eq!(config.regression_threshold, 0.5);
    assert_eq!(config.correction_penalty, 0.05);
    assert_eq!(config.success_boost, 0.01);
}

#[test]
fn trust_config_serde_roundtrip() {
    let config = TrustConfig {
        initial_score: 0.9,
        decay_half_life_days: 45.0,
        regression_threshold: 0.6,
        correction_penalty: 0.03,
        success_boost: 0.02,
    };
    let json = serde_json::to_string(&config).unwrap();
    let deserialized: TrustConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config.initial_score, deserialized.initial_score);
    assert_eq!(
        config.decay_half_life_days,
        deserialized.decay_half_life_days
    );
    assert_eq!(
        config.regression_threshold,
        deserialized.regression_threshold
    );
    assert_eq!(config.correction_penalty, deserialized.correction_penalty);
    assert_eq!(config.success_boost, deserialized.success_boost);
}

#[test]
fn trust_config_partial_serde_uses_defaults() {
    let json = r#"{"initial_score": 0.9}"#;
    let config: TrustConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.initial_score, 0.9);
    assert_eq!(config.decay_half_life_days, 30.0); // default
    assert_eq!(config.regression_threshold, 0.5); // default
}

// ── TrustScore Tests ───────────────────────────────────────────

#[test]
fn trust_score_serde_roundtrip() {
    let now = Utc::now();
    let score = TrustScore {
        domain: "code-review".to_string(),
        score: 0.75,
        last_updated: now,
        event_count: 42,
    };
    let json = serde_json::to_string(&score).unwrap();
    let deserialized: TrustScore = serde_json::from_str(&json).unwrap();
    assert_eq!(score.domain, deserialized.domain);
    assert_eq!(score.score, deserialized.score);
    assert_eq!(score.event_count, deserialized.event_count);
}

// ── CorrectionEvent Tests ──────────────────────────────────────

#[test]
fn correction_event_serde_roundtrip() {
    let now = Utc::now();
    let event = CorrectionEvent {
        domain: "deployment".to_string(),
        correction_type: CorrectionType::UserOverride,
        description: "User rejected proposed change".to_string(),
        timestamp: now,
    };
    let json = serde_json::to_string(&event).unwrap();
    let deserialized: CorrectionEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event.domain, deserialized.domain);
    assert_eq!(event.correction_type, deserialized.correction_type);
    assert_eq!(event.description, deserialized.description);
}

#[test]
fn correction_event_type_serde_as_snake_case() {
    let json_override = serde_json::to_string(&CorrectionType::UserOverride).unwrap();
    assert_eq!(json_override, r#""user_override""#);

    let json_quality = serde_json::to_string(&CorrectionType::QualityFailure).unwrap();
    assert_eq!(json_quality, r#""quality_failure""#);

    let json_sop = serde_json::to_string(&CorrectionType::SopDeviation).unwrap();
    assert_eq!(json_sop, r#""sop_deviation""#);

    let deserialized: CorrectionType = serde_json::from_str(r#""user_override""#).unwrap();
    assert_eq!(deserialized, CorrectionType::UserOverride);
}

// ── RegressionAlert Tests ──────────────────────────────────────

#[test]
fn regression_alert_serde_roundtrip() {
    let now = Utc::now();
    let alert = RegressionAlert {
        domain: "testing".to_string(),
        current_score: 0.45,
        threshold: 0.5,
        detected_at: now,
    };
    let json = serde_json::to_string(&alert).unwrap();
    let deserialized: RegressionAlert = serde_json::from_str(&json).unwrap();
    assert_eq!(alert.domain, deserialized.domain);
    assert_eq!(alert.current_score, deserialized.current_score);
    assert_eq!(alert.threshold, deserialized.threshold);
}

// ── TrustTracker Initialization Tests ──────────────────────────

#[test]
fn trust_tracker_new_initializes_empty() {
    let config = TrustConfig::default();
    let tracker = TrustTracker::new(config);
    assert_eq!(tracker.snapshot().len(), 0);
    assert_eq!(tracker.correction_log().len(), 0);
}

#[test]
fn trust_tracker_get_score_missing_domain_returns_initial() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    let score = tracker.get_score("new-domain");
    assert_eq!(score, 0.8); // default initial_score
    assert_eq!(tracker.snapshot().len(), 1);
}

// ── Correction Recording Tests ─────────────────────────────────

#[test]
fn record_correction_reduces_score() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // initialize at 0.8
    tracker.record_correction("domain1", CorrectionType::UserOverride, "test correction");
    let score = tracker.get_score("domain1");
    assert!((score - 0.75).abs() < 0.001); // 0.8 - 0.05 = 0.75
}

#[test]
fn record_correction_score_floor_at_zero() {
    let config = TrustConfig {
        correction_penalty: 1.0,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1");
    tracker.record_correction("domain1", CorrectionType::QualityFailure, "big penalty");
    let score = tracker.get_score("domain1");
    assert_eq!(score, 0.0); // floored at 0.0
}

#[test]
fn record_correction_updates_timestamp() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    let _ = tracker.get_score("domain1");

    std::thread::sleep(std::time::Duration::from_millis(10));

    let before = Utc::now();
    tracker.record_correction("domain1", CorrectionType::SopDeviation, "test");
    let after = Utc::now();

    let snapshot = tracker.snapshot();
    let updated_time = snapshot["domain1"].last_updated;
    assert!(updated_time >= before && updated_time <= after);
}

#[test]
fn record_correction_increments_event_count() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 0);

    tracker.record_correction("domain1", CorrectionType::UserOverride, "event 1");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 1);

    tracker.record_correction("domain1", CorrectionType::QualityFailure, "event 2");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 2);
}

#[test]
fn record_correction_logs_event() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.record_correction("domain1", CorrectionType::UserOverride, "user rejected");

    let log = tracker.correction_log();
    assert_eq!(log.len(), 1);
    let event = &log[0];
    assert_eq!(event.domain, "domain1");
    assert_eq!(event.correction_type, CorrectionType::UserOverride);
    assert_eq!(event.description, "user rejected");
}

#[test]
fn record_correction_multiple_events_cumulative_penalty() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8

    tracker.record_correction("domain1", CorrectionType::UserOverride, "first");
    assert!((tracker.get_score("domain1") - 0.75).abs() < 0.001); // 0.8 - 0.05

    tracker.record_correction("domain1", CorrectionType::QualityFailure, "second");
    assert!((tracker.get_score("domain1") - 0.70).abs() < 0.001); // 0.75 - 0.05

    tracker.record_correction("domain1", CorrectionType::SopDeviation, "third");
    assert!((tracker.get_score("domain1") - 0.65).abs() < 0.001); // 0.70 - 0.05
}

// ── Success Recording Tests ────────────────────────────────────

#[test]
fn record_success_increases_score() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8
    tracker.record_success("domain1");
    let score = tracker.get_score("domain1");
    assert!((score - 0.81).abs() < 0.001); // 0.8 + 0.01
}

#[test]
fn record_success_score_ceiling_at_one() {
    let config = TrustConfig {
        success_boost: 0.5,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8
    tracker.record_success("domain1");
    let score = tracker.get_score("domain1");
    assert_eq!(score, 1.0); // capped at 1.0
}

#[test]
fn record_success_updates_timestamp() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1");

    std::thread::sleep(std::time::Duration::from_millis(10));

    let before = Utc::now();
    tracker.record_success("domain1");
    let after = Utc::now();

    let snapshot = tracker.snapshot();
    let updated_time = snapshot["domain1"].last_updated;
    assert!(updated_time >= before && updated_time <= after);
}

#[test]
fn record_success_increments_event_count() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 0);

    tracker.record_success("domain1");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 1);

    tracker.record_success("domain1");
    assert_eq!(tracker.snapshot()["domain1"].event_count, 2);
}

#[test]
fn record_success_multiple_events_cumulative_boost() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8

    tracker.record_success("domain1");
    assert!((tracker.get_score("domain1") - 0.81).abs() < 0.001);

    tracker.record_success("domain1");
    assert!((tracker.get_score("domain1") - 0.82).abs() < 0.001);

    // Many successes eventually cap at 1.0
    for _ in 0..20 {
        tracker.record_success("domain1");
    }
    assert_eq!(tracker.get_score("domain1"), 1.0);
}

// ── Decay Logic Tests ──────────────────────────────────────────

#[test]
fn apply_decay_toward_initial_score_above() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    // Boost score above initial
    tracker.get_score("domain1");
    for _ in 0..30 {
        tracker.record_success("domain1");
    }
    let high_score = tracker.get_score("domain1");
    assert!(high_score > 0.8); // above initial

    // Apply decay after 30 days (half-life)
    let past = tracker.snapshot()["domain1"].last_updated;
    let future = past + Duration::days(30);
    tracker.apply_decay(future);

    let decayed_score = tracker.get_score("domain1");
    // After one half-life, score should be halfway between current and initial
    let expected = 0.8 + (high_score - 0.8) * 0.5;
    assert!((decayed_score - expected).abs() < 0.01);
}

#[test]
fn apply_decay_toward_initial_score_below() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    // Lower score below initial
    tracker.get_score("domain1");
    for _ in 0..10 {
        tracker.record_correction("domain1", CorrectionType::UserOverride, "test");
    }
    let low_score = tracker.get_score("domain1");
    assert!(low_score < 0.8); // below initial

    // Apply decay after 30 days (half-life)
    let past = tracker.snapshot()["domain1"].last_updated;
    let future = past + Duration::days(30);
    tracker.apply_decay(future);

    let decayed_score = tracker.get_score("domain1");
    // Score should move toward initial
    let expected = 0.8 + (low_score - 0.8) * 0.5;
    assert!((decayed_score - expected).abs() < 0.01);
}

#[test]
fn apply_decay_half_life_math() {
    let config = TrustConfig {
        decay_half_life_days: 10.0,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..20 {
        tracker.record_success("domain1");
    }
    let initial = tracker.get_score("domain1");
    let start_time = tracker.snapshot()["domain1"].last_updated;

    // After 10 days (one half-life), score moves halfway to initial_score
    let after_half_life = start_time + Duration::days(10);
    tracker.apply_decay(after_half_life);

    let after_decay = tracker.get_score("domain1");
    let expected = 0.8 + (initial - 0.8) * 0.5;
    assert!((after_decay - expected).abs() < 0.01);
}

#[test]
fn apply_decay_no_change_when_at_initial() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // exactly at initial_score

    let past = tracker.snapshot()["domain1"].last_updated;
    let future = past + Duration::days(30);
    tracker.apply_decay(future);

    let score = tracker.get_score("domain1");
    assert!((score - 0.8).abs() < 0.001); // unchanged
}

#[test]
fn apply_decay_updates_last_updated() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1");

    let past = tracker.snapshot()["domain1"].last_updated;
    let future = past + Duration::days(30);
    tracker.apply_decay(future);

    let snapshot = tracker.snapshot();
    let updated = snapshot["domain1"].last_updated;
    assert_eq!(updated, future);
}

#[test]
fn apply_decay_multiple_domains() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    tracker.get_score("domain2");
    tracker.record_success("domain1");
    tracker.record_correction("domain2", CorrectionType::UserOverride, "test");

    let past = Utc::now();
    let future = past + Duration::days(30);
    tracker.apply_decay(future);

    // Both should have been updated
    let snapshot = tracker.snapshot();
    assert_eq!(snapshot["domain1"].last_updated, future);
    assert_eq!(snapshot["domain2"].last_updated, future);
}

// ── Regression Detection Tests ─────────────────────────────────

#[test]
fn check_regression_below_threshold_returns_alert() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..10 {
        tracker.record_correction("domain1", CorrectionType::UserOverride, "test");
    }

    let alert = tracker.check_regression("domain1");
    assert!(alert.is_some());
    let alert = alert.unwrap();
    assert_eq!(alert.domain, "domain1");
    assert!(alert.current_score < 0.5);
    assert_eq!(alert.threshold, 0.5);
}

#[test]
fn check_regression_above_threshold_returns_none() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8 > 0.5

    let alert = tracker.check_regression("domain1");
    assert!(alert.is_none());
}

#[test]
fn check_regression_alert_fields_correct() {
    let config = TrustConfig {
        regression_threshold: 0.6,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..5 {
        tracker.record_correction("domain1", CorrectionType::QualityFailure, "test");
    }

    let current_score = tracker.get_score("domain1");
    let alert = tracker.check_regression("domain1").unwrap();

    assert_eq!(alert.domain, "domain1");
    assert!((alert.current_score - current_score).abs() < 0.001);
    assert_eq!(alert.threshold, 0.6);
}

#[test]
fn check_regression_missing_domain_uses_initial() {
    let config = TrustConfig {
        initial_score: 0.9,
        regression_threshold: 0.5,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);

    // New domain has initial_score 0.9, which is > 0.5
    let alert = tracker.check_regression("new-domain");
    assert!(alert.is_none());
}

// ── Autonomy Level Reduction Tests ─────────────────────────────

#[test]
fn get_effective_autonomy_no_regression_returns_base() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);
    tracker.get_score("domain1"); // 0.8 > 0.5, no regression

    assert_eq!(tracker.get_effective_autonomy("domain1", "full"), "full");
    assert_eq!(
        tracker.get_effective_autonomy("domain1", "supervised"),
        "supervised"
    );
    assert_eq!(
        tracker.get_effective_autonomy("domain1", "read_only"),
        "read_only"
    );
}

#[test]
fn get_effective_autonomy_regression_reduces_full_to_supervised() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..10 {
        tracker.record_correction("domain1", CorrectionType::UserOverride, "test");
    }

    assert_eq!(
        tracker.get_effective_autonomy("domain1", "full"),
        "supervised"
    );
}

#[test]
fn get_effective_autonomy_regression_reduces_supervised_to_readonly() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..10 {
        tracker.record_correction("domain1", CorrectionType::UserOverride, "test");
    }

    assert_eq!(
        tracker.get_effective_autonomy("domain1", "supervised"),
        "read_only"
    );
}

#[test]
fn get_effective_autonomy_regression_readonly_stays_readonly() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    for _ in 0..10 {
        tracker.record_correction("domain1", CorrectionType::UserOverride, "test");
    }

    assert_eq!(
        tracker.get_effective_autonomy("domain1", "read_only"),
        "read_only"
    );
}

#[test]
fn get_effective_autonomy_missing_domain_uses_initial() {
    let config = TrustConfig {
        initial_score: 0.9,
        regression_threshold: 0.5,
        ..Default::default()
    };
    let mut tracker = TrustTracker::new(config);

    // New domain has initial_score 0.9 > 0.5, no regression
    assert_eq!(tracker.get_effective_autonomy("new-domain", "full"), "full");
}

// ── Diagnostics Tests ──────────────────────────────────────────

#[test]
fn corrections_for_domain_filters() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.record_correction("domain1", CorrectionType::UserOverride, "d1-event1");
    tracker.record_correction("domain2", CorrectionType::QualityFailure, "d2-event1");
    tracker.record_correction("domain1", CorrectionType::SopDeviation, "d1-event2");

    let domain1_events = tracker.corrections_for_domain("domain1");
    assert_eq!(domain1_events.len(), 2);
    assert_eq!(domain1_events[0].description, "d1-event1");
    assert_eq!(domain1_events[1].description, "d1-event2");

    let domain2_events = tracker.corrections_for_domain("domain2");
    assert_eq!(domain2_events.len(), 1);
    assert_eq!(domain2_events[0].description, "d2-event1");
}

#[test]
fn snapshot_returns_all_scores() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("domain1");
    tracker.get_score("domain2");
    tracker.record_success("domain1");

    let snapshot = tracker.snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.contains_key("domain1"));
    assert!(snapshot.contains_key("domain2"));
    assert!((snapshot["domain1"].score - 0.81).abs() < 0.001);
    assert!((snapshot["domain2"].score - 0.8).abs() < 0.001);
}

#[test]
fn domains_returns_all_tracked_domains() {
    let config = TrustConfig::default();
    let mut tracker = TrustTracker::new(config);

    tracker.get_score("alpha");
    tracker.get_score("beta");
    tracker.get_score("gamma");

    let mut domains = tracker.domains();
    domains.sort();
    assert_eq!(domains, vec!["alpha", "beta", "gamma"]);
}
