//! Feed scheduler — manages scheduled execution of feeds using tokio timers.
//!
//! Loads active feeds from the Aria registry, parses their cron/interval
//! schedules, and spawns repeating tokio tasks for each feed.

use crate::aria::db::AriaDb;
use crate::feed::executor::FeedExecutor;
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use uuid::Uuid;

/// A feed that has been scheduled for periodic execution.
struct ScheduledFeed {
    handle: JoinHandle<()>,
    interval_secs: u64,
}

/// Manages the lifecycle of scheduled feed executions.
///
/// On `start()`, loads all active feeds from the registry and spawns
/// a tokio task for each one that fires on the computed interval.
pub struct FeedScheduler {
    db: AriaDb,
    executor: Arc<FeedExecutor>,
    jobs: Arc<Mutex<HashMap<String, ScheduledFeed>>>,
    running: Arc<AtomicBool>,
}

impl FeedScheduler {
    pub fn new(db: AriaDb, executor: FeedExecutor) -> Self {
        Self {
            db,
            executor: Arc::new(executor),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Load all active feeds from registry and schedule each.
    pub async fn start(&self) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        self.sync_all().await?;
        tracing::info!("Feed scheduler started");
        Ok(())
    }

    /// Clear all timers and stop scheduling.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        let mut jobs = self.jobs.lock().unwrap();
        for (_id, scheduled) in jobs.drain() {
            scheduled.handle.abort();
        }
        tracing::info!("Feed scheduler stopped");
    }

    /// Sync all feeds from registry — schedule active, remove stale.
    pub async fn sync_all(&self) -> Result<()> {
        let feed_ids = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM aria_feeds WHERE status = 'active'",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut ids = Vec::new();
            for r in rows {
                ids.push(r?);
            }
            Ok(ids)
        })?;

        // Remove jobs for feeds that are no longer active
        {
            let jobs = self.jobs.lock().unwrap();
            let active_set: std::collections::HashSet<&str> =
                feed_ids.iter().map(String::as_str).collect();
            let stale: Vec<String> = jobs
                .keys()
                .filter(|k| !active_set.contains(k.as_str()))
                .cloned()
                .collect();
            drop(jobs);
            for id in stale {
                self.remove_feed(&id);
            }
        }

        // Schedule each active feed
        for feed_id in &feed_ids {
            if let Err(e) = self.sync_feed(feed_id).await {
                tracing::warn!(feed_id = feed_id, error = %e, "Failed to sync feed");
            }
        }

        Ok(())
    }

    /// Schedule or reschedule a single feed.
    pub async fn sync_feed(&self, feed_id: &str) -> Result<()> {
        // Load feed config from DB
        let feed_data = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, schedule, handler_code, retention, status
                 FROM aria_feeds WHERE id = ?1",
            )?;
            let result = stmt.query_row(params![feed_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            });
            match result {
                Ok(data) => Ok(Some(data)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })?;

        let Some((id, tenant_id, schedule, handler_code, retention_json, status)) = feed_data
        else {
            self.remove_feed(feed_id);
            return Ok(());
        };

        if status != "active" {
            self.remove_feed(feed_id);
            return Ok(());
        }

        let interval_secs = parse_schedule_to_interval(&schedule);

        // Parse retention config
        let (max_items, max_age_days) = if let Some(ref json) = retention_json {
            let retention: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
            (
                retention["max_items"].as_u64().map(|v| v as u32),
                retention["max_age_days"].as_u64().map(|v| v as u32),
            )
        } else {
            (None, None)
        };

        // Remove existing job if present
        self.remove_feed(feed_id);

        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Spawn the timer task
        let executor = self.executor.clone();
        let running = self.running.clone();
        let feed_id_owned = id.clone();
        let tenant_id_owned = tenant_id.clone();
        let handler_code_owned = handler_code.clone();

        let handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(interval_secs));
            // Skip the initial immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                if !running.load(Ordering::SeqCst) {
                    break;
                }

                let run_id = Uuid::new_v4().to_string();
                tracing::debug!(
                    feed_id = feed_id_owned.as_str(),
                    run_id = run_id.as_str(),
                    "Feed timer fired"
                );

                match executor
                    .execute(
                        &feed_id_owned,
                        &tenant_id_owned,
                        &handler_code_owned,
                        &run_id,
                    )
                    .await
                {
                    Ok(result) => {
                        if result.success && !result.items.is_empty() {
                            if let Err(e) = executor.store_items(
                                &tenant_id_owned,
                                &feed_id_owned,
                                &run_id,
                                &result.items,
                            ) {
                                tracing::warn!(
                                    feed_id = feed_id_owned.as_str(),
                                    error = %e,
                                    "Failed to store feed items"
                                );
                            }
                        }

                        // Apply retention policy
                        if max_items.is_some() || max_age_days.is_some() {
                            if let Err(e) = executor.prune_by_retention(
                                &feed_id_owned,
                                max_items,
                                max_age_days,
                            ) {
                                tracing::warn!(
                                    feed_id = feed_id_owned.as_str(),
                                    error = %e,
                                    "Failed to prune feed items"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            feed_id = feed_id_owned.as_str(),
                            error = %e,
                            "Feed execution failed"
                        );
                    }
                }
            }
        });

        let mut jobs = self.jobs.lock().unwrap();
        jobs.insert(
            id,
            ScheduledFeed {
                handle,
                interval_secs,
            },
        );

        tracing::info!(
            feed_id = feed_id,
            interval_secs = interval_secs,
            "Scheduled feed"
        );

        Ok(())
    }

    /// Unschedule a feed by aborting its timer task.
    pub fn remove_feed(&self, feed_id: &str) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(scheduled) = jobs.remove(feed_id) {
            scheduled.handle.abort();
            tracing::debug!(feed_id = feed_id, "Removed scheduled feed");
        }
    }

    /// Run a feed immediately (one-shot, outside the timer cycle).
    pub async fn execute_feed(&self, feed_id: &str) -> Result<()> {
        let feed_data = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, handler_code, retention
                 FROM aria_feeds WHERE id = ?1 AND status = 'active'",
            )?;
            let result = stmt.query_row(params![feed_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            });
            match result {
                Ok(data) => Ok(Some(data)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })?;

        let (id, tenant_id, handler_code, retention_json) =
            feed_data.context("Feed not found or not active")?;

        let run_id = Uuid::new_v4().to_string();
        let result = self
            .executor
            .execute(&id, &tenant_id, &handler_code, &run_id)
            .await?;

        if result.success && !result.items.is_empty() {
            self.executor
                .store_items(&tenant_id, &id, &run_id, &result.items)?;
        }

        // Apply retention
        if let Some(ref json) = retention_json {
            let retention: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
            let max_items = retention["max_items"].as_u64().map(|v| v as u32);
            let max_age_days = retention["max_age_days"].as_u64().map(|v| v as u32);
            if max_items.is_some() || max_age_days.is_some() {
                self.executor
                    .prune_by_retention(&id, max_items, max_age_days)?;
            }
        }

        Ok(())
    }

    /// Get the number of currently scheduled feeds.
    pub fn scheduled_count(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    /// Check if a specific feed is currently scheduled.
    pub fn is_scheduled(&self, feed_id: &str) -> bool {
        self.jobs.lock().unwrap().contains_key(feed_id)
    }
}

/// Parse a cron-like schedule expression into an interval in seconds.
///
/// Supported patterns:
/// - `*/N * * * *` -> every N minutes
/// - `M * * * *` -> every hour (3600s)
/// - `M H * * *` -> every day (86400s)
/// - Fallback: every hour (3600s)
pub fn parse_schedule_to_interval(schedule: &str) -> u64 {
    let parts: Vec<&str> = schedule.trim().split_whitespace().collect();

    if parts.len() < 5 {
        return 3600; // Default: every hour
    }

    let minute_field = parts[0];
    let hour_field = parts[1];
    let day_field = parts[2];
    let month_field = parts[3];
    let weekday_field = parts[4];

    // Check for */N pattern in minute field
    if let Some(stripped) = minute_field.strip_prefix("*/") {
        if let Ok(n) = stripped.parse::<u64>() {
            if n > 0 && hour_field == "*" && day_field == "*" && month_field == "*" && weekday_field == "*" {
                return n * 60;
            }
        }
    }

    // Specific minute, wildcard hour -> every hour
    if minute_field.parse::<u64>().is_ok()
        && hour_field == "*"
        && day_field == "*"
        && month_field == "*"
        && weekday_field == "*"
    {
        return 3600;
    }

    // Specific minute + specific hour, wildcard day -> every day
    if minute_field.parse::<u64>().is_ok()
        && hour_field.parse::<u64>().is_ok()
        && day_field == "*"
        && month_field == "*"
        && weekday_field == "*"
    {
        return 86400;
    }

    // Default fallback: every hour
    3600
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::feed::executor::FeedExecutor;
    use chrono::Utc;
    use rusqlite::params;

    fn setup() -> (AriaDb, FeedScheduler) {
        let db = AriaDb::open_in_memory().unwrap();
        let executor = FeedExecutor::new(db.clone());
        let scheduler = FeedScheduler::new(db.clone(), executor);
        (db, scheduler)
    }

    fn insert_feed(db: &AriaDb, id: &str, schedule: &str, status: &str) {
        let now = Utc::now().to_rfc3339();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_feeds (id, tenant_id, name, schedule, handler_code, status, created_at, updated_at)
                 VALUES (?1, 'test-tenant', ?2, ?3, 'console.log(\"hi\")', ?4, ?5, ?5)",
                params![id, format!("feed-{id}"), schedule, status, now],
            )?;
            Ok(())
        })
        .unwrap();
    }

    // ── Schedule parsing tests ─────────────────────────────────

    #[test]
    fn parse_every_n_minutes() {
        assert_eq!(parse_schedule_to_interval("*/5 * * * *"), 300);
        assert_eq!(parse_schedule_to_interval("*/1 * * * *"), 60);
        assert_eq!(parse_schedule_to_interval("*/15 * * * *"), 900);
        assert_eq!(parse_schedule_to_interval("*/30 * * * *"), 1800);
    }

    #[test]
    fn parse_every_hour() {
        assert_eq!(parse_schedule_to_interval("0 * * * *"), 3600);
        assert_eq!(parse_schedule_to_interval("30 * * * *"), 3600);
    }

    #[test]
    fn parse_every_day() {
        assert_eq!(parse_schedule_to_interval("0 9 * * *"), 86400);
        assert_eq!(parse_schedule_to_interval("30 14 * * *"), 86400);
    }

    #[test]
    fn parse_fallback_for_complex_expressions() {
        // Specific day of week -> fallback to hourly
        assert_eq!(parse_schedule_to_interval("0 9 * * 1"), 3600);
        // Specific month -> fallback to hourly
        assert_eq!(parse_schedule_to_interval("0 0 1 1 *"), 3600);
    }

    #[test]
    fn parse_fallback_for_invalid_input() {
        assert_eq!(parse_schedule_to_interval("invalid"), 3600);
        assert_eq!(parse_schedule_to_interval(""), 3600);
        assert_eq!(parse_schedule_to_interval("* *"), 3600);
    }

    // ── Scheduler lifecycle tests ──────────────────────────────

    #[tokio::test]
    async fn start_and_stop_lifecycle() {
        let (_db, scheduler) = setup();
        scheduler.start().await.unwrap();
        assert_eq!(scheduler.scheduled_count(), 0); // No feeds in DB
        scheduler.stop();
    }

    #[tokio::test]
    async fn sync_all_schedules_active_feeds() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");
        insert_feed(&db, "f2", "*/10 * * * *", "active");
        insert_feed(&db, "f3", "*/1 * * * *", "paused");

        scheduler.start().await.unwrap();
        assert_eq!(scheduler.scheduled_count(), 2);
        assert!(scheduler.is_scheduled("f1"));
        assert!(scheduler.is_scheduled("f2"));
        assert!(!scheduler.is_scheduled("f3"));
        scheduler.stop();
    }

    #[tokio::test]
    async fn sync_feed_adds_new_job() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");

        scheduler.running.store(true, Ordering::SeqCst);
        scheduler.sync_feed("f1").await.unwrap();
        assert!(scheduler.is_scheduled("f1"));
        scheduler.stop();
    }

    #[tokio::test]
    async fn sync_feed_removes_inactive() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");

        scheduler.start().await.unwrap();
        assert!(scheduler.is_scheduled("f1"));

        // Mark feed as paused
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_feeds SET status = 'paused' WHERE id = 'f1'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        scheduler.sync_feed("f1").await.unwrap();
        assert!(!scheduler.is_scheduled("f1"));
        scheduler.stop();
    }

    #[tokio::test]
    async fn remove_feed_unschedules() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");

        scheduler.start().await.unwrap();
        assert!(scheduler.is_scheduled("f1"));

        scheduler.remove_feed("f1");
        assert!(!scheduler.is_scheduled("f1"));
        assert_eq!(scheduler.scheduled_count(), 0);
        scheduler.stop();
    }

    #[tokio::test]
    async fn execute_feed_runs_once() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");

        // execute_feed should succeed even without starting scheduler
        scheduler.execute_feed("f1").await.unwrap();
    }

    #[tokio::test]
    async fn execute_feed_errors_for_missing_feed() {
        let (_db, scheduler) = setup();
        let result = scheduler.execute_feed("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stop_aborts_all_jobs() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");
        insert_feed(&db, "f2", "*/10 * * * *", "active");

        scheduler.start().await.unwrap();
        assert_eq!(scheduler.scheduled_count(), 2);

        scheduler.stop();
        assert_eq!(scheduler.scheduled_count(), 0);
    }

    #[tokio::test]
    async fn sync_all_removes_stale_jobs() {
        let (db, scheduler) = setup();
        insert_feed(&db, "f1", "*/5 * * * *", "active");
        insert_feed(&db, "f2", "*/10 * * * *", "active");

        scheduler.start().await.unwrap();
        assert_eq!(scheduler.scheduled_count(), 2);

        // Delete f2 from DB
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_feeds SET status = 'deleted' WHERE id = 'f2'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        scheduler.sync_all().await.unwrap();
        assert_eq!(scheduler.scheduled_count(), 1);
        assert!(scheduler.is_scheduled("f1"));
        assert!(!scheduler.is_scheduled("f2"));
        scheduler.stop();
    }
}
