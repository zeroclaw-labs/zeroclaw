//! SQLite-backed channel statistics shared by the channel runtime and gateway.
//!
//! Stores channel-level aggregates only. No sender IDs, message bodies, or
//! other user-identifying metadata are persisted here.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

const CHANNEL_STATS_DB_NAME: &str = "channel_stats.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatsSnapshot {
    pub channel_name: String,
    pub inbound_count: u64,
    pub outbound_count: u64,
    pub message_count: u64,
    pub last_inbound_at: Option<String>,
    pub last_outbound_at: Option<String>,
    pub last_message_at: Option<String>,
}

pub struct ChannelStatsStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl ChannelStatsStore {
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).context("Failed to create sessions directory")?;

        let db_path = sessions_dir.join(CHANNEL_STATS_DB_NAME);
        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open channel stats DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 4194304;",
        )
        .context("Failed to configure channel stats SQLite pragmas")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channel_stats (
                channel_name TEXT PRIMARY KEY,
                inbound_count INTEGER NOT NULL DEFAULT 0,
                outbound_count INTEGER NOT NULL DEFAULT 0,
                last_inbound_at TEXT,
                last_outbound_at TEXT,
                updated_at TEXT NOT NULL
            );",
        )
        .context("Failed to initialize channel stats schema")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
        }

        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn record_inbound(&self, channel_name: &str) -> Result<()> {
        self.record_message(channel_name, MessageDirection::Inbound)
    }

    pub fn record_outbound(&self, channel_name: &str) -> Result<()> {
        self.record_message(channel_name, MessageDirection::Outbound)
    }

    pub fn snapshot_all(&self) -> Result<Vec<ChannelStatsSnapshot>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT channel_name, inbound_count, outbound_count, last_inbound_at, last_outbound_at
                 FROM channel_stats
                 ORDER BY channel_name ASC",
            )
            .context("Failed to prepare channel stats snapshot query")?;

        let rows = stmt
            .query_map([], |row| {
                let last_inbound_at: Option<String> = row.get(3)?;
                let last_outbound_at: Option<String> = row.get(4)?;
                Ok(ChannelStatsSnapshot {
                    channel_name: row.get(0)?,
                    inbound_count: row.get(1)?,
                    outbound_count: row.get(2)?,
                    message_count: row.get::<_, u64>(1)? + row.get::<_, u64>(2)?,
                    last_message_at: max_timestamp(
                        last_inbound_at.as_deref(),
                        last_outbound_at.as_deref(),
                    ),
                    last_inbound_at,
                    last_outbound_at,
                })
            })
            .context("Failed to read channel stats rows")?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to decode channel stats rows")
    }

    fn record_message(&self, channel_name: &str, direction: MessageDirection) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let normalized = normalize_channel_name(channel_name);
        let conn = self.conn.lock();

        match direction {
            MessageDirection::Inbound => {
                conn.execute(
                    "INSERT INTO channel_stats (
                        channel_name,
                        inbound_count,
                        outbound_count,
                        last_inbound_at,
                        last_outbound_at,
                        updated_at
                    ) VALUES (?1, 1, 0, ?2, NULL, ?2)
                    ON CONFLICT(channel_name) DO UPDATE SET
                        inbound_count = inbound_count + 1,
                        last_inbound_at = excluded.last_inbound_at,
                        updated_at = excluded.updated_at",
                    params![normalized, now],
                )
                .with_context(|| format!("Failed to record inbound stats for {normalized}"))?;
            }
            MessageDirection::Outbound => {
                conn.execute(
                    "INSERT INTO channel_stats (
                        channel_name,
                        inbound_count,
                        outbound_count,
                        last_inbound_at,
                        last_outbound_at,
                        updated_at
                    ) VALUES (?1, 0, 1, NULL, ?2, ?2)
                    ON CONFLICT(channel_name) DO UPDATE SET
                        outbound_count = outbound_count + 1,
                        last_outbound_at = excluded.last_outbound_at,
                        updated_at = excluded.updated_at",
                    params![normalized, now],
                )
                .with_context(|| format!("Failed to record outbound stats for {normalized}"))?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum MessageDirection {
    Inbound,
    Outbound,
}

pub fn normalize_channel_name(channel_name: &str) -> &str {
    channel_name
        .split_once(':')
        .map(|(base, _)| base)
        .unwrap_or(channel_name)
}

fn max_timestamp(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (parse_timestamp(left), parse_timestamp(right)) {
        (Some(left_ts), Some(right_ts)) => Some(if left_ts >= right_ts {
            left_ts.to_rfc3339()
        } else {
            right_ts.to_rfc3339()
        }),
        (Some(ts), None) | (None, Some(ts)) => Some(ts.to_rfc3339()),
        (None, None) => None,
    }
}

fn parse_timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value.and_then(|raw| {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|ts| ts.with_timezone(&Utc))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn record_and_snapshot_accumulate_counts() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStatsStore::new(tmp.path()).unwrap();

        store.record_inbound("slack").unwrap();
        store.record_inbound("slack:room-1").unwrap();
        store.record_outbound("slack").unwrap();

        let snapshots = store.snapshot_all().unwrap();
        assert_eq!(snapshots.len(), 1);

        let slack = &snapshots[0];
        assert_eq!(slack.channel_name, "slack");
        assert_eq!(slack.inbound_count, 2);
        assert_eq!(slack.outbound_count, 1);
        assert_eq!(slack.message_count, 3);
        assert!(slack.last_inbound_at.is_some());
        assert!(slack.last_outbound_at.is_some());
        assert!(slack.last_message_at.is_some());
    }

    #[test]
    fn normalize_channel_name_drops_room_suffix() {
        assert_eq!(normalize_channel_name("matrix:!room:server"), "matrix");
        assert_eq!(normalize_channel_name("webhook"), "webhook");
    }

    #[test]
    fn store_creates_private_database_under_sessions_dir() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStatsStore::new(tmp.path()).unwrap();

        assert_eq!(
            store.db_path(),
            tmp.path().join("sessions").join(CHANNEL_STATS_DB_NAME)
        );
        assert!(store.db_path().exists());
    }
}
