use crate::config::Config;
use crate::taskqueue::{TaskItem, TaskPatch, TaskPriority, TaskSource, TaskStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use uuid::Uuid;

pub fn enqueue(config: &Config, item: &TaskItem) -> Result<TaskItem> {
    with_connection(config, |conn| {
        let deps_json = serde_json::to_string(&item.dependencies)?;
        conn.execute(
            "INSERT INTO task_queue (id, created_at, priority, status, source, task, dependencies, result, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                item.id,
                item.created_at.to_rfc3339(),
                i64::from(item.priority.value()),
                item.status.as_str(),
                item.source.as_str(),
                item.task,
                deps_json,
                item.result,
                item.completed_at.map(|d| d.to_rfc3339()),
            ],
        )
        .context("Failed to enqueue task")?;
        Ok(())
    })?;
    get(config, &item.id)
}

pub fn dequeue(config: &Config) -> Result<Option<TaskItem>> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, created_at, priority, status, source, task, dependencies, result, completed_at
             FROM task_queue
             WHERE status = 'pending'
             ORDER BY priority DESC, created_at ASC
             LIMIT 100",
        )?;

        let rows = stmt.query_map([], map_task_row)?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row?);
        }

        let processing_ids: Vec<String> = {
            let mut pstmt = conn
                .prepare("SELECT id FROM task_queue WHERE status IN ('pending', 'processing')")?;
            let prows = pstmt.query_map([], |row| row.get(0))?;
            let mut ids = Vec::new();
            for r in prows {
                ids.push(r?);
            }
            ids
        };

        for candidate in &candidates {
            let deps_met = candidate
                .dependencies
                .iter()
                .all(|dep_id| !processing_ids.contains(dep_id));
            if deps_met {
                conn.execute(
                    "UPDATE task_queue SET status = 'processing' WHERE id = ?1 AND status = 'pending'",
                    params![candidate.id],
                )
                .context("Failed to atomically dequeue task")?;

                return Ok(Some(TaskItem {
                    status: TaskStatus::Processing,
                    ..candidate.clone()
                }));
            }
        }

        Ok(None)
    })
}

pub fn get(config: &Config, id: &str) -> Result<TaskItem> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, created_at, priority, status, source, task, dependencies, result, completed_at
             FROM task_queue WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            map_task_row(row).map_err(Into::into)
        } else {
            anyhow::bail!("Task '{id}' not found")
        }
    })
}

pub fn update(config: &Config, id: &str, patch: TaskPatch) -> Result<TaskItem> {
    let mut item = get(config, id)?;

    if let Some(priority) = patch.priority {
        item.priority = priority;
    }
    if let Some(status) = patch.status {
        item.status = status;
    }
    if let Some(source) = patch.source {
        item.source = source;
    }
    if let Some(task) = patch.task {
        item.task = task;
    }
    if let Some(dependencies) = patch.dependencies {
        item.dependencies = dependencies;
    }
    if let Some(result) = patch.result {
        item.result = Some(result);
    }
    if let Some(completed_at) = patch.completed_at {
        item.completed_at = Some(completed_at);
    }

    let deps_json = serde_json::to_string(&item.dependencies)?;
    with_connection(config, |conn| {
        conn.execute(
            "UPDATE task_queue
             SET priority = ?1, status = ?2, source = ?3, task = ?4, dependencies = ?5, result = ?6, completed_at = ?7
             WHERE id = ?8",
            params![
                i64::from(item.priority.value()),
                item.status.as_str(),
                item.source.as_str(),
                item.task,
                deps_json,
                item.result,
                item.completed_at.map(|d| d.to_rfc3339()),
                item.id,
            ],
        )
        .context("Failed to update task")?;
        Ok(())
    })?;

    get(config, id)
}

pub fn list(config: &Config, status_filter: Option<TaskStatus>) -> Result<Vec<TaskItem>> {
    with_connection(config, |conn| {
        let (sql, filter_val) = match status_filter {
            Some(s) => (
                "SELECT id, created_at, priority, status, source, task, dependencies, result, completed_at
                 FROM task_queue WHERE status = ?1 ORDER BY priority DESC, created_at ASC",
                Some(s.as_str().to_string()),
            ),
            None => (
                "SELECT id, created_at, priority, status, source, task, dependencies, result, completed_at
                 FROM task_queue ORDER BY priority DESC, created_at ASC",
                None,
            ),
        };

        let mut items = Vec::new();
        if let Some(val) = filter_val {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map(params![val], map_task_row)?;
            for row in rows {
                items.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], map_task_row)?;
            for row in rows {
                items.push(row?);
            }
        }

        Ok(items)
    })
}

pub fn pending_count(config: &Config) -> Result<i64> {
    with_connection(config, |conn| {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task_queue WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    })
}

pub fn fail(config: &Config, id: &str, reason: &str) -> Result<TaskItem> {
    let now = Utc::now();
    with_connection(config, |conn| {
        let changed = conn.execute(
            "UPDATE task_queue SET status = 'failed', result = ?1, completed_at = ?2 WHERE id = ?3",
            params![reason, now.to_rfc3339(), id],
        )
        .context("Failed to mark task as failed")?;
        if changed == 0 {
            anyhow::bail!("Task '{id}' not found");
        }
        Ok(())
    })?;
    get(config, id)
}

pub fn complete(config: &Config, id: &str, result: &str) -> Result<TaskItem> {
    let now = Utc::now();
    with_connection(config, |conn| {
        let changed = conn.execute(
            "UPDATE task_queue SET status = 'completed', result = ?1, completed_at = ?2 WHERE id = ?3",
            params![result, now.to_rfc3339(), id],
        )
        .context("Failed to mark task as completed")?;
        if changed == 0 {
            anyhow::bail!("Task '{id}' not found");
        }
        Ok(())
    })?;
    get(config, id)
}

pub fn new_task_item(
    task: &str,
    priority: TaskPriority,
    source: TaskSource,
    dependencies: Vec<String>,
) -> TaskItem {
    TaskItem {
        id: Uuid::new_v4().to_string(),
        created_at: Utc::now(),
        priority,
        status: TaskStatus::Pending,
        source,
        task: task.to_string(),
        dependencies,
        result: None,
        completed_at: None,
    }
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp in task queue DB: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

fn sql_conversion_error(err: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

fn map_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskItem> {
    let created_at_raw: String = row.get(1)?;
    let deps_raw: String = row.get(6)?;
    let completed_at_raw: Option<String> = row.get(8)?;

    let dependencies: Vec<String> = serde_json::from_str(&deps_raw).unwrap_or_default();

    Ok(TaskItem {
        id: row.get(0)?,
        created_at: parse_rfc3339(&created_at_raw).map_err(sql_conversion_error)?,
        priority: TaskPriority::new(
            u8::try_from(row.get::<_, i64>(2).unwrap_or(5).clamp(1, 10)).unwrap_or(5),
        ),
        status: TaskStatus::parse(&row.get::<_, String>(3)?),
        source: TaskSource::parse(&row.get::<_, String>(4)?),
        task: row.get(5)?,
        dependencies,
        result: row.get(7)?,
        completed_at: match completed_at_raw {
            Some(raw) => Some(parse_rfc3339(&raw).map_err(sql_conversion_error)?),
            None => None,
        },
    })
}

fn with_connection<T>(config: &Config, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = config.workspace_dir.join("taskqueue").join("tasks.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create taskqueue directory: {}", parent.display())
        })?;
    }

    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open task queue DB: {}", db_path.display()))?;

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS task_queue (
            id           TEXT PRIMARY KEY,
            created_at   TEXT NOT NULL,
            priority     INTEGER NOT NULL DEFAULT 5,
            status       TEXT NOT NULL DEFAULT 'pending',
            source       TEXT NOT NULL DEFAULT 'user',
            task         TEXT NOT NULL,
            dependencies TEXT NOT NULL DEFAULT '[]',
            result       TEXT,
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_task_queue_status ON task_queue(status);
        CREATE INDEX IF NOT EXISTS idx_task_queue_priority ON task_queue(priority);",
    )
    .context("Failed to initialize task queue schema")?;

    f(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn enqueue_and_get_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let item = new_task_item("test task", TaskPriority::new(7), TaskSource::User, vec![]);
        let stored = enqueue(&config, &item).unwrap();
        assert_eq!(stored.task, "test task");
        assert_eq!(stored.priority.value(), 7);
        assert_eq!(stored.status, TaskStatus::Pending);

        let fetched = get(&config, &stored.id).unwrap();
        assert_eq!(fetched.id, stored.id);
    }

    #[test]
    fn dequeue_returns_highest_priority_first() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let low = new_task_item("low", TaskPriority::new(1), TaskSource::User, vec![]);
        let high = new_task_item("high", TaskPriority::new(10), TaskSource::User, vec![]);
        enqueue(&config, &low).unwrap();
        enqueue(&config, &high).unwrap();

        let dequeued = dequeue(&config).unwrap().unwrap();
        assert_eq!(dequeued.task, "high");
        assert_eq!(dequeued.status, TaskStatus::Processing);
    }

    #[test]
    fn dequeue_respects_dependencies() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let blocker = new_task_item("blocker", TaskPriority::new(5), TaskSource::User, vec![]);
        let blocked = new_task_item(
            "blocked",
            TaskPriority::new(10),
            TaskSource::User,
            vec![blocker.id.clone()],
        );
        enqueue(&config, &blocker).unwrap();
        enqueue(&config, &blocked).unwrap();

        let dequeued = dequeue(&config).unwrap().unwrap();
        assert_eq!(dequeued.task, "blocker");
    }

    #[test]
    fn dequeue_empty_returns_none() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = dequeue(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_with_status_filter() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let item = new_task_item("task", TaskPriority::new(5), TaskSource::User, vec![]);
        enqueue(&config, &item).unwrap();

        let pending = list(&config, Some(TaskStatus::Pending)).unwrap();
        assert_eq!(pending.len(), 1);

        let completed = list(&config, Some(TaskStatus::Completed)).unwrap();
        assert!(completed.is_empty());
    }

    #[test]
    fn pending_count_tracks_correctly() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        assert_eq!(pending_count(&config).unwrap(), 0);

        let item = new_task_item("task", TaskPriority::new(5), TaskSource::User, vec![]);
        enqueue(&config, &item).unwrap();
        assert_eq!(pending_count(&config).unwrap(), 1);

        dequeue(&config).unwrap();
        assert_eq!(pending_count(&config).unwrap(), 0);
    }

    #[test]
    fn fail_and_complete_set_status_and_result() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let item1 = new_task_item("will_fail", TaskPriority::new(5), TaskSource::User, vec![]);
        enqueue(&config, &item1).unwrap();
        let failed = fail(&config, &item1.id, "some error").unwrap();
        assert_eq!(failed.status, TaskStatus::Failed);
        assert_eq!(failed.result.as_deref(), Some("some error"));
        assert!(failed.completed_at.is_some());

        let item2 = new_task_item("will_pass", TaskPriority::new(5), TaskSource::User, vec![]);
        enqueue(&config, &item2).unwrap();
        let completed = complete(&config, &item2.id, "done").unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert_eq!(completed.result.as_deref(), Some("done"));
        assert!(completed.completed_at.is_some());
    }

    #[test]
    fn update_modifies_fields() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let item = new_task_item("original", TaskPriority::new(3), TaskSource::User, vec![]);
        enqueue(&config, &item).unwrap();

        let updated = update(
            &config,
            &item.id,
            TaskPatch {
                priority: Some(TaskPriority::new(9)),
                task: Some("modified".to_string()),
                ..TaskPatch::default()
            },
        )
        .unwrap();

        assert_eq!(updated.priority.value(), 9);
        assert_eq!(updated.task, "modified");
    }

    #[test]
    fn priority_clamps_to_range() {
        assert_eq!(TaskPriority::new(0).value(), 1);
        assert_eq!(TaskPriority::new(11).value(), 10);
        assert_eq!(TaskPriority::new(5).value(), 5);
    }
}
