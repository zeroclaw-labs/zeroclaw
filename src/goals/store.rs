use crate::config::Config;
use crate::goals::{Goal, GoalSource, GoalStatus};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;

pub fn propose(config: &Config, goal: &Goal) -> Result<Goal> {
    with_connection(config, |conn| {
        conn.execute(
            "INSERT INTO goals (id, title, description, source, status, priority, proposed_at, approved_at, completed_at, evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                goal.id,
                goal.title,
                goal.description,
                goal.source.as_str(),
                goal.status.as_str(),
                i64::from(goal.priority),
                goal.proposed_at.to_rfc3339(),
                goal.approved_at.map(|d| d.to_rfc3339()),
                goal.completed_at.map(|d| d.to_rfc3339()),
                goal.evidence,
            ],
        )
        .context("Failed to propose goal")?;
        Ok(())
    })?;
    get(config, &goal.id)
}

pub fn list(config: &Config, status_filter: Option<GoalStatus>) -> Result<Vec<Goal>> {
    with_connection(config, |conn| {
        let mut goals = Vec::new();

        match status_filter {
            Some(s) => {
                let mut stmt = conn.prepare(
                    "SELECT id, title, description, source, status, priority, proposed_at, approved_at, completed_at, evidence
                     FROM goals WHERE status = ?1 ORDER BY priority DESC, proposed_at ASC",
                )?;
                let rows = stmt.query_map(params![s.as_str()], map_goal_row)?;
                for row in rows {
                    goals.push(row?);
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, title, description, source, status, priority, proposed_at, approved_at, completed_at, evidence
                     FROM goals ORDER BY priority DESC, proposed_at ASC",
                )?;
                let rows = stmt.query_map([], map_goal_row)?;
                for row in rows {
                    goals.push(row?);
                }
            }
        }

        Ok(goals)
    })
}

pub fn get(config: &Config, id: &str) -> Result<Goal> {
    with_connection(config, |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, title, description, source, status, priority, proposed_at, approved_at, completed_at, evidence
             FROM goals WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            map_goal_row(row).map_err(Into::into)
        } else {
            anyhow::bail!("Goal '{id}' not found")
        }
    })
}

pub fn approve(config: &Config, id: &str) -> Result<Goal> {
    let now = Utc::now();
    with_connection(config, |conn| {
        let changed = conn
            .execute(
                "UPDATE goals SET status = 'approved', approved_at = ?1 WHERE id = ?2",
                params![now.to_rfc3339(), id],
            )
            .context("Failed to approve goal")?;
        if changed == 0 {
            anyhow::bail!("Goal '{id}' not found");
        }
        Ok(())
    })?;
    get(config, id)
}

pub fn reject(config: &Config, id: &str, reason: &str) -> Result<Goal> {
    with_connection(config, |conn| {
        let changed = conn
            .execute(
                "UPDATE goals SET status = 'rejected', evidence = ?1 WHERE id = ?2",
                params![reason, id],
            )
            .context("Failed to reject goal")?;
        if changed == 0 {
            anyhow::bail!("Goal '{id}' not found");
        }
        Ok(())
    })?;
    get(config, id)
}

pub fn complete(config: &Config, id: &str, evidence: &str) -> Result<Goal> {
    let now = Utc::now();
    with_connection(config, |conn| {
        let changed = conn.execute(
            "UPDATE goals SET status = 'completed', completed_at = ?1, evidence = ?2 WHERE id = ?3",
            params![now.to_rfc3339(), evidence, id],
        )
        .context("Failed to complete goal")?;
        if changed == 0 {
            anyhow::bail!("Goal '{id}' not found");
        }
        Ok(())
    })?;
    get(config, id)
}

#[derive(Debug, Serialize)]
pub struct GoalTrackingSummary {
    pub total: i64,
    pub proposed: i64,
    pub approved: i64,
    pub in_progress: i64,
    pub completed: i64,
    pub rejected: i64,
}

pub fn track(config: &Config) -> Result<GoalTrackingSummary> {
    with_connection(config, |conn| {
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM goals", [], |row| row.get(0))?;
        let proposed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM goals WHERE status = 'proposed'",
            [],
            |row| row.get(0),
        )?;
        let approved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM goals WHERE status = 'approved'",
            [],
            |row| row.get(0),
        )?;
        let in_progress: i64 = conn.query_row(
            "SELECT COUNT(*) FROM goals WHERE status = 'in_progress'",
            [],
            |row| row.get(0),
        )?;
        let completed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM goals WHERE status = 'completed'",
            [],
            |row| row.get(0),
        )?;
        let rejected: i64 = conn.query_row(
            "SELECT COUNT(*) FROM goals WHERE status = 'rejected'",
            [],
            |row| row.get(0),
        )?;

        Ok(GoalTrackingSummary {
            total,
            proposed,
            approved,
            in_progress,
            completed,
            rejected,
        })
    })
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("Invalid RFC3339 timestamp in goals DB: {raw}"))?;
    Ok(parsed.with_timezone(&Utc))
}

fn sql_conversion_error(err: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(err.into())
}

fn map_goal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Goal> {
    let proposed_at_raw: String = row.get(6)?;
    let approved_at_raw: Option<String> = row.get(7)?;
    let completed_at_raw: Option<String> = row.get(8)?;

    Ok(Goal {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        source: GoalSource::parse(&row.get::<_, String>(3)?),
        status: GoalStatus::parse(&row.get::<_, String>(4)?),
        priority: u8::try_from(row.get::<_, i64>(5).unwrap_or(5).clamp(1, 10)).unwrap_or(5),
        proposed_at: parse_rfc3339(&proposed_at_raw).map_err(sql_conversion_error)?,
        approved_at: match approved_at_raw {
            Some(raw) => Some(parse_rfc3339(&raw).map_err(sql_conversion_error)?),
            None => None,
        },
        completed_at: match completed_at_raw {
            Some(raw) => Some(parse_rfc3339(&raw).map_err(sql_conversion_error)?),
            None => None,
        },
        evidence: row.get(9)?,
    })
}

fn with_connection<T>(config: &Config, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = config.workspace_dir.join("goals").join("goals.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create goals directory: {}", parent.display()))?;
    }

    let conn = Connection::open(&db_path)
        .with_context(|| format!("Failed to open goals DB: {}", db_path.display()))?;

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS goals (
            id           TEXT PRIMARY KEY,
            title        TEXT NOT NULL,
            description  TEXT NOT NULL,
            source       TEXT NOT NULL DEFAULT 'telos',
            status       TEXT NOT NULL DEFAULT 'proposed',
            priority     INTEGER NOT NULL DEFAULT 5,
            proposed_at  TEXT NOT NULL,
            approved_at  TEXT,
            completed_at TEXT,
            evidence     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);
        CREATE INDEX IF NOT EXISTS idx_goals_priority ON goals(priority);",
    )
    .context("Failed to initialize goals schema")?;

    f(&conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::goals::GoalSource;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    fn new_goal(title: &str, priority: u8, source: GoalSource) -> Goal {
        Goal {
            id: Uuid::new_v4().to_string(),
            title: title.to_string(),
            description: format!("Description for {title}"),
            source,
            status: GoalStatus::Proposed,
            priority,
            proposed_at: Utc::now(),
            approved_at: None,
            completed_at: None,
            evidence: None,
        }
    }

    #[test]
    fn propose_and_get_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let goal = new_goal("test goal", 7, GoalSource::Telos);
        let stored = propose(&config, &goal).unwrap();
        assert_eq!(stored.title, "test goal");
        assert_eq!(stored.priority, 7);
        assert_eq!(stored.status, GoalStatus::Proposed);

        let fetched = get(&config, &stored.id).unwrap();
        assert_eq!(fetched.id, stored.id);
    }

    #[test]
    fn approve_sets_status_and_timestamp() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let goal = new_goal("approvable", 5, GoalSource::UserRequest);
        propose(&config, &goal).unwrap();

        let approved = approve(&config, &goal.id).unwrap();
        assert_eq!(approved.status, GoalStatus::Approved);
        assert!(approved.approved_at.is_some());
    }

    #[test]
    fn reject_sets_status_and_reason() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let goal = new_goal("rejectable", 3, GoalSource::StalePrd);
        propose(&config, &goal).unwrap();

        let rejected = reject(&config, &goal.id, "not aligned").unwrap();
        assert_eq!(rejected.status, GoalStatus::Rejected);
        assert_eq!(rejected.evidence.as_deref(), Some("not aligned"));
    }

    #[test]
    fn complete_sets_status_evidence_and_timestamp() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let goal = new_goal("completable", 8, GoalSource::RecurringFailure);
        propose(&config, &goal).unwrap();

        let completed = complete(&config, &goal.id, "tests passing").unwrap();
        assert_eq!(completed.status, GoalStatus::Completed);
        assert_eq!(completed.evidence.as_deref(), Some("tests passing"));
        assert!(completed.completed_at.is_some());
    }

    #[test]
    fn list_with_status_filter() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let g1 = new_goal("g1", 5, GoalSource::Telos);
        let g2 = new_goal("g2", 5, GoalSource::Telos);
        propose(&config, &g1).unwrap();
        propose(&config, &g2).unwrap();
        approve(&config, &g1.id).unwrap();

        let proposed = list(&config, Some(GoalStatus::Proposed)).unwrap();
        assert_eq!(proposed.len(), 1);

        let approved = list(&config, Some(GoalStatus::Approved)).unwrap();
        assert_eq!(approved.len(), 1);

        let all = list(&config, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn track_returns_correct_summary() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let g1 = new_goal("g1", 5, GoalSource::Telos);
        let g2 = new_goal("g2", 5, GoalSource::Telos);
        let g3 = new_goal("g3", 5, GoalSource::Telos);
        propose(&config, &g1).unwrap();
        propose(&config, &g2).unwrap();
        propose(&config, &g3).unwrap();
        approve(&config, &g1.id).unwrap();
        complete(&config, &g2.id, "done").unwrap();

        let summary = track(&config).unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.proposed, 1);
        assert_eq!(summary.approved, 1);
        assert_eq!(summary.completed, 1);
        assert_eq!(summary.rejected, 0);
        assert_eq!(summary.in_progress, 0);
    }

    #[test]
    fn get_nonexistent_goal_fails() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let result = get(&config, "nonexistent-id");
        assert!(result.is_err());
    }
}
