use super::*;
use tempfile::TempDir;
use zeroclaw_api::cost::{ConversationCostReason as Reason, ConversationCostStatus as Status};

fn config() -> CostConfig {
    CostConfig {
        enabled: true,
        track_per_agent: true,
        ..Default::default()
    }
}

fn usage(model: &str, dollars: f64) -> TokenUsage {
    TokenUsage::new(model, 1_000_000, 0, 0, dollars, 0.0, 0.0)
}

fn record(tracker: &CostTracker, conversation: &str, agent: &str, amount: f64) {
    tracker
        .record_observed_usage(
            usage("model", amount),
            UsageCompleteness::Complete,
            Some(agent),
            Some(conversation.into()),
        )
        .unwrap();
}

#[test]
fn conversation_totals_isolate_chats_and_include_delegates_once_after_restart() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    record(&tracker, "rpc_a", "parent", 1.0);
    record(&tracker, "rpc_b", "parent", 20.0);
    record(&tracker, "rpc_a", "delegate", 2.0);
    tracker
        .record_usage_attributed(usage("unattributed", 50.0), Some("parent"), None, None)
        .unwrap();
    assert_eq!(
        tracker
            .get_summary_for_conversation("rpc_a")
            .unwrap()
            .estimated_cost_usd,
        Some(3.0)
    );
    assert_eq!(
        tracker
            .get_summary_for_conversation("rpc_b")
            .unwrap()
            .estimated_cost_usd,
        Some(20.0)
    );
    assert_eq!(
        tracker
            .get_summary_for_agent("delegate")
            .unwrap()
            .session_cost_usd,
        2.0
    );
    let restarted = CostTracker::new(config(), tmp.path()).unwrap();
    let summary = restarted.get_summary_for_conversation("rpc_a").unwrap();
    assert_eq!(summary.estimated_cost_usd, Some(3.0));
    assert_eq!(summary.status, Status::Estimated);
    assert_eq!(restarted.get_summary().unwrap().session_cost_usd, 0.0);
}

#[test]
fn conversation_gap_survives_restart_without_changing_existing_rollups() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    record(&tracker, "rpc_a", "parent", 1.0);
    tracker
        .record_observed_usage(
            TokenUsage::new("missing", 0, 0, 0, 0.0, 0.0, 0.0),
            UsageCompleteness::Unavailable,
            Some("child"),
            Some("rpc_a".into()),
        )
        .unwrap();
    let summary = tracker.get_summary_for_conversation("rpc_a").unwrap();
    assert_eq!(summary.estimated_cost_usd, Some(1.0));
    assert_eq!(summary.status, Status::Partial);
    assert_eq!(summary.reasons, vec![Reason::MissingUsage]);
    assert_eq!(tracker.get_summary().unwrap().request_count, 1);
    let restarted = CostTracker::new(config(), tmp.path()).unwrap();
    assert_eq!(
        restarted.get_summary_for_conversation("rpc_a").unwrap(),
        summary
    );
    let rollup = restarted.get_summary_in_bounds(None, None).unwrap();
    assert_eq!(rollup.request_count, 1);
    assert!(!rollup.by_model.contains_key("missing"));
    assert!(!rollup.by_agent.contains_key("child"));
}

#[test]
fn conversation_unknown_is_not_free_and_lower_bounds_are_partial() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    assert_eq!(
        tracker
            .get_summary_for_conversation("new")
            .unwrap()
            .estimated_cost_usd,
        None
    );
    let mut unknown = usage("unknown", 0.0);
    unknown.unpriced_tokens = unknown.total_tokens;
    unknown.pricing_available = false;
    tracker
        .record_observed_usage(
            unknown,
            UsageCompleteness::Complete,
            None,
            Some("unknown".into()),
        )
        .unwrap();
    let summary = tracker.get_summary_for_conversation("unknown").unwrap();
    assert_eq!(summary.status, Status::Unavailable);
    assert_eq!(summary.estimated_cost_usd, None);
    assert!(summary.reasons.contains(&Reason::MissingPrices));
    record(&tracker, "free", "parent", 0.0);
    assert_eq!(
        tracker
            .get_summary_for_conversation("free")
            .unwrap()
            .estimated_cost_usd,
        Some(0.0)
    );
    tracker
        .record_observed_usage(
            usage("interrupted", 0.5),
            UsageCompleteness::LowerBound,
            None,
            Some("partial".into()),
        )
        .unwrap();
    let summary = tracker.get_summary_for_conversation("partial").unwrap();
    assert_eq!(summary.estimated_cost_usd, Some(0.5));
    assert_eq!(summary.status, Status::Partial);
}

#[test]
fn conversation_legacy_corruption_and_disabled_tracking_are_explicit() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    tracker
        .record_usage_attributed(usage("legacy", 1.0), None, None, Some("legacy".into()))
        .unwrap();
    assert_eq!(
        tracker
            .get_summary_for_conversation("legacy")
            .unwrap()
            .reasons,
        vec![Reason::UnverifiedHistory]
    );
    let path = tracker.lock_storage().path.clone();
    writeln!(
        OpenOptions::new().append(true).open(path).unwrap(),
        "broken"
    )
    .unwrap();
    let summary = tracker.get_summary_for_conversation("legacy").unwrap();
    assert_eq!(summary.estimated_cost_usd, Some(1.0));
    assert!(summary.reasons.contains(&Reason::LedgerIntegrity));
    tracker.config.write().enabled = false;
    assert_eq!(
        tracker.get_summary_for_conversation("legacy").unwrap(),
        ConversationCostSummary::unavailable(Reason::TrackingDisabled)
    );
}

#[test]
fn conversation_append_and_sync_failures_latch_health_without_retrying_charges() {
    fn fail_sync(_: &File) -> std::io::Result<()> {
        Err(std::io::Error::other("forced sync failure"))
    }
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    let row = CostRecord::new("process", usage("model", 1.0))
        .with_conversation_id(Some("chat".into()))
        .with_usage_completeness(UsageCompleteness::Complete);
    assert!(
        tracker
            .record_cost_record_with_sync(row, true, fail_sync)
            .is_err()
    );
    assert_eq!(
        tracker
            .get_summary_for_conversation("chat")
            .unwrap()
            .reasons,
        vec![Reason::PersistenceUncertain]
    );
    record(&tracker, "chat", "parent", 2.0);
    assert_eq!(
        tracker
            .get_summary_for_conversation("chat")
            .unwrap()
            .estimated_cost_usd,
        None
    );
    // The failed sync did append. Reopening observes exactly those two rows.
    let restarted = CostTracker::new(config(), tmp.path()).unwrap();
    assert_eq!(
        restarted
            .get_summary_for_conversation("chat")
            .unwrap()
            .estimated_cost_usd,
        Some(3.0)
    );
    let other = TempDir::new().unwrap();
    let broken = CostTracker::new(config(), other.path()).unwrap();
    fs::create_dir(broken.lock_storage().path.clone()).unwrap();
    assert!(
        broken
            .record_observed_usage(
                usage("model", 1.0),
                UsageCompleteness::Complete,
                None,
                Some("chat".into())
            )
            .is_err()
    );
    assert_eq!(
        broken.get_summary_for_conversation("chat").unwrap().reasons,
        vec![Reason::PersistenceUncertain]
    );
}

#[test]
fn conversation_snapshot_releases_append_lock_and_excludes_concurrent_append() {
    let tmp = TempDir::new().unwrap();
    let tracker = Arc::new(CostTracker::new(config(), tmp.path()).unwrap());
    record(&tracker, "chat", "parent", 1.0);
    // A representative larger ledger without fsync per fixture row.
    let path = tracker.lock_storage().path.clone();
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    for _ in 0..10_000 {
        let row =
            CostRecord::new("old", usage("other", 1.0)).with_conversation_id(Some("other".into()));
        writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
    }
    drop(file);
    let writer = Arc::clone(&tracker);
    let snapshot = tracker
        .conversation_summary_at_snapshot("chat", || {
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                record(&writer, "chat", "child", 2.0);
                tx.send(()).unwrap();
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("snapshot reader must release append lock");
            thread.join().unwrap();
        })
        .unwrap();
    assert_eq!(snapshot.estimated_cost_usd, Some(1.0));
    assert_eq!(
        tracker
            .get_summary_for_conversation("chat")
            .unwrap()
            .estimated_cost_usd,
        Some(3.0)
    );
}

#[test]
fn conversation_invalid_numbers_and_unreadable_storage_never_look_free() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    let path = tracker.lock_storage().path.clone();
    let mut invalid = usage("invalid", 0.0);
    invalid.total_tokens = 0;
    let row = CostRecord::new("old", invalid)
        .with_conversation_id(Some("chat".into()))
        .with_usage_completeness(UsageCompleteness::Complete);
    fs::write(&path, format!("{}\n", serde_json::to_string(&row).unwrap())).unwrap();
    let summary = tracker.get_summary_for_conversation("chat").unwrap();
    assert_eq!(summary.estimated_cost_usd, None);
    assert_eq!(summary.status, Status::Unavailable);
    assert_eq!(summary.reasons, vec![Reason::LedgerIntegrity]);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(tracker.get_summary_for_conversation("chat").is_err());
}

#[test]
fn conversation_rejected_nonfinite_write_invalidates_prior_estimate() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    record(&tracker, "chat", "parent", 1.0);
    let mut invalid = usage("invalid", 0.0);
    invalid.cost_usd = f64::NAN;
    assert!(
        tracker
            .record_observed_usage(
                invalid,
                UsageCompleteness::Complete,
                None,
                Some("chat".into())
            )
            .is_err()
    );
    assert_eq!(
        tracker.get_summary_for_conversation("chat").unwrap(),
        ConversationCostSummary::unavailable(Reason::PersistenceUncertain)
    );
}

#[test]
fn conversation_keeps_saved_cached_and_mixed_model_prices() {
    let tmp = TempDir::new().unwrap();
    let tracker = CostTracker::new(config(), tmp.path()).unwrap();
    let first = TokenUsage::new("cached", 1_000_000, 500_000, 500_000, 2.0, 4.0, 1.0);
    let expected = first.cost_usd + 3.0;
    tracker
        .record_observed_usage(
            first,
            UsageCompleteness::Complete,
            None,
            Some("chat".into()),
        )
        .unwrap();
    // The next attempt uses a different model/rate. Reads only sum saved prices.
    tracker
        .record_observed_usage(
            usage("replacement", 3.0),
            UsageCompleteness::Complete,
            None,
            Some("chat".into()),
        )
        .unwrap();
    let mut next_config = config();
    next_config.daily_limit_usd = 9999.0;
    tracker.update_config(next_config);
    let summary = CostTracker::new(config(), tmp.path())
        .unwrap()
        .get_summary_for_conversation("chat")
        .unwrap();
    assert_eq!(summary.estimated_cost_usd, Some(expected));
    assert_eq!(summary.status, Status::Estimated);
}
