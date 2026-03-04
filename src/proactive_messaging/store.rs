use super::types::{MessagePriority, MessageStatus, QueuedMessage};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn db_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir
        .join("proactive_messaging")
        .join("queue.db")
}

fn with_connection<T>(workspace_dir: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let path = db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create proactive_messaging directory: {}",
                parent.display()
            )
        })?;
    }

    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open proactive messaging DB: {}", path.display()))?;

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS outbound_queue (
            id              TEXT PRIMARY KEY,
            channel         TEXT NOT NULL,
            recipient       TEXT NOT NULL,
            message         TEXT NOT NULL,
            priority        TEXT NOT NULL DEFAULT 'normal',
            reason_queued   TEXT,
            source_context  TEXT,
            created_at      TEXT NOT NULL,
            expires_at      TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'pending',
            sent_at         TEXT,
            error           TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_oq_status ON outbound_queue(status);
         CREATE INDEX IF NOT EXISTS idx_oq_recipient_status ON outbound_queue(recipient, status);
         CREATE INDEX IF NOT EXISTS idx_oq_expires_at ON outbound_queue(expires_at);",
    )
    .context("Failed to initialize proactive messaging schema")?;

    f(&conn)
}

/// Enqueue a message for later delivery.
pub fn enqueue_message(
    workspace_dir: &Path,
    channel: &str,
    recipient: &str,
    message: &str,
    priority: MessagePriority,
    reason_queued: Option<&str>,
    source_context: Option<&str>,
    ttl_hours: u64,
) -> Result<QueuedMessage> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let expires_at = now + Duration::hours(ttl_hours as i64);

    with_connection(workspace_dir, |conn| {
        conn.execute(
            "INSERT INTO outbound_queue
                (id, channel, recipient, message, priority, reason_queued, source_context, created_at, expires_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending')",
            params![
                id,
                channel,
                recipient,
                message,
                priority.as_str(),
                reason_queued,
                source_context,
                now.to_rfc3339(),
                expires_at.to_rfc3339(),
            ],
        )
        .context("Failed to enqueue proactive message")?;
        Ok(())
    })?;

    Ok(QueuedMessage {
        id,
        channel: channel.to_string(),
        recipient: recipient.to_string(),
        message: message.to_string(),
        priority,
        reason_queued: reason_queued.map(String::from),
        source_context: source_context.map(String::from),
        created_at: now,
        expires_at,
        status: MessageStatus::Pending,
    })
}

/// List all pending messages, optionally filtered by recipient and/or channel.
pub fn list_pending(
    workspace_dir: &Path,
    recipient: Option<&str>,
    channel: Option<&str>,
) -> Result<Vec<QueuedMessage>> {
    with_connection(workspace_dir, |conn| {
        let mut sql = String::from(
            "SELECT id, channel, recipient, message, priority, reason_queued, source_context,
                    created_at, expires_at, status
             FROM outbound_queue WHERE status = 'pending'",
        );
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(r) = recipient {
            sql.push_str(" AND recipient = ?");
            bind_values.push(Box::new(r.to_string()));
        }
        if let Some(c) = channel {
            sql.push_str(" AND channel = ?");
            bind_values.push(Box::new(c.to_string()));
        }
        sql.push_str(" ORDER BY created_at ASC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), row_to_queued_message)?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    })
}

/// Get pending messages for a specific recipient on a specific channel.
pub fn pending_for_recipient(
    workspace_dir: &Path,
    channel: &str,
    recipient: &str,
) -> Result<Vec<QueuedMessage>> {
    list_pending(workspace_dir, Some(recipient), Some(channel))
}

/// Mark one or more messages as sent.
pub fn mark_sent(workspace_dir: &Path, ids: &[&str]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let now = Utc::now().to_rfc3339();
    with_connection(workspace_dir, |conn| {
        let mut count = 0usize;
        for id in ids {
            count += conn.execute(
                "UPDATE outbound_queue SET status = 'sent', sent_at = ?1 WHERE id = ?2 AND status = 'pending'",
                params![now, id],
            )?;
        }
        Ok(count)
    })
}

/// Clear (mark as cleared) messages by IDs, or all pending if `ids` is `None`.
pub fn clear_messages(
    workspace_dir: &Path,
    ids: Option<&[&str]>,
    recipient: Option<&str>,
    channel: Option<&str>,
) -> Result<usize> {
    with_connection(workspace_dir, |conn| {
        if let Some(ids) = ids {
            let mut count = 0usize;
            for id in ids {
                count += conn.execute(
                    "UPDATE outbound_queue SET status = 'cleared' WHERE id = ?1 AND status = 'pending'",
                    params![id],
                )?;
            }
            Ok(count)
        } else {
            let mut sql =
                String::from("UPDATE outbound_queue SET status = 'cleared' WHERE status = 'pending'");
            let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            if let Some(r) = recipient {
                sql.push_str(" AND recipient = ?");
                bind_values.push(Box::new(r.to_string()));
            }
            if let Some(c) = channel {
                sql.push_str(" AND channel = ?");
                bind_values.push(Box::new(c.to_string()));
            }
            let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                bind_values.iter().map(|b| b.as_ref()).collect();
            let count = conn.execute(&sql, params_refs.as_slice())?;
            Ok(count)
        }
    })
}

/// Fetch pending messages eligible for drain delivery (status = pending, not expired).
pub fn drain_due(workspace_dir: &Path) -> Result<Vec<QueuedMessage>> {
    let now = Utc::now().to_rfc3339();
    with_connection(workspace_dir, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, channel, recipient, message, priority, reason_queued, source_context,
                    created_at, expires_at, status
             FROM outbound_queue
             WHERE status = 'pending' AND expires_at > ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![now], row_to_queued_message)?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    })
}

/// Expire stale messages whose TTL has passed.
pub fn expire_stale(workspace_dir: &Path) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    with_connection(workspace_dir, |conn| {
        let count = conn.execute(
            "UPDATE outbound_queue SET status = 'expired' WHERE status = 'pending' AND expires_at <= ?1",
            params![now],
        )?;
        Ok(count)
    })
}

/// Count messages sent within a time window (for rate limiting).
pub fn count_sent_in_window(workspace_dir: &Path, since: DateTime<Utc>) -> Result<u32> {
    with_connection(workspace_dir, |conn| {
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM outbound_queue WHERE status = 'sent' AND sent_at >= ?1",
            params![since.to_rfc3339()],
            |row| row.get(0),
        )?;
        Ok(count)
    })
}

/// Count pending messages in the queue.
pub fn count_pending(workspace_dir: &Path) -> Result<usize> {
    with_connection(workspace_dir, |conn| {
        let count: usize = conn.query_row(
            "SELECT COUNT(*) FROM outbound_queue WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    })
}

fn row_to_queued_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedMessage> {
    let priority_str: String = row.get(4)?;
    let status_str: String = row.get(9)?;
    let created_at_str: String = row.get(7)?;
    let expires_at_str: String = row.get(8)?;

    Ok(QueuedMessage {
        id: row.get(0)?,
        channel: row.get(1)?,
        recipient: row.get(2)?,
        message: row.get(3)?,
        priority: priority_str
            .parse()
            .unwrap_or(MessagePriority::Normal),
        reason_queued: row.get(5)?,
        source_context: row.get(6)?,
        created_at: DateTime::parse_from_rfc3339(&created_at_str)
            .unwrap_or_else(|_| DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z").unwrap())
            .with_timezone(&Utc),
        expires_at: DateTime::parse_from_rfc3339(&expires_at_str)
            .unwrap_or_else(|_| DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z").unwrap())
            .with_timezone(&Utc),
        status: status_str
            .parse()
            .unwrap_or(MessageStatus::Pending),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn enqueue_and_list_pending() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        let msg = enqueue_message(
            ws,
            "signal",
            "+1234567890",
            "Test message",
            MessagePriority::Normal,
            Some("quiet hours"),
            Some("cron:abc test-job"),
            24,
        )
        .unwrap();

        assert_eq!(msg.status, MessageStatus::Pending);
        assert_eq!(msg.channel, "signal");

        let pending = list_pending(ws, None, None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, msg.id);
    }

    #[test]
    fn mark_sent_and_count() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        let msg = enqueue_message(
            ws,
            "telegram",
            "123",
            "Hello",
            MessagePriority::Normal,
            None,
            None,
            24,
        )
        .unwrap();

        let count = mark_sent(ws, &[&msg.id]).unwrap();
        assert_eq!(count, 1);

        let pending = list_pending(ws, None, None).unwrap();
        assert!(pending.is_empty());

        let sent_count =
            count_sent_in_window(ws, Utc::now() - Duration::hours(1)).unwrap();
        assert_eq!(sent_count, 1);
    }

    #[test]
    fn clear_messages_by_id() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        let msg = enqueue_message(
            ws, "signal", "+1", "m1", MessagePriority::Normal, None, None, 24,
        )
        .unwrap();

        let count = clear_messages(ws, Some(&[&msg.id]), None, None).unwrap();
        assert_eq!(count, 1);

        let pending = list_pending(ws, None, None).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn expire_stale_messages() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        // Enqueue with 0-hour TTL so it's already expired
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expired_at = now - Duration::hours(1);

        with_connection(ws, |conn| {
            conn.execute(
                "INSERT INTO outbound_queue
                    (id, channel, recipient, message, priority, created_at, expires_at, status)
                 VALUES (?1, 'sig', '+1', 'old', 'normal', ?2, ?3, 'pending')",
                params![id, now.to_rfc3339(), expired_at.to_rfc3339()],
            )?;
            Ok(())
        })
        .unwrap();

        let expired = expire_stale(ws).unwrap();
        assert_eq!(expired, 1);

        let pending = list_pending(ws, None, None).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn count_pending_messages() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        enqueue_message(ws, "sig", "+1", "a", MessagePriority::Normal, None, None, 24).unwrap();
        enqueue_message(ws, "sig", "+1", "b", MessagePriority::High, None, None, 24).unwrap();

        assert_eq!(count_pending(ws).unwrap(), 2);
    }

    #[test]
    fn filter_by_recipient_and_channel() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();

        enqueue_message(ws, "signal", "+1", "a", MessagePriority::Normal, None, None, 24).unwrap();
        enqueue_message(ws, "telegram", "99", "b", MessagePriority::Normal, None, None, 24)
            .unwrap();

        let signal_only = list_pending(ws, None, Some("signal")).unwrap();
        assert_eq!(signal_only.len(), 1);
        assert_eq!(signal_only[0].channel, "signal");

        let recipient_only = list_pending(ws, Some("99"), None).unwrap();
        assert_eq!(recipient_only.len(), 1);
        assert_eq!(recipient_only[0].recipient, "99");
    }
}
