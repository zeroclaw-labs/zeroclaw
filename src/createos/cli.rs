use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use super::CreateOsDb;

/// createOS subcommands for Augusta CLI.
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CreateOsCommands {
    /// List tasks with optional filters
    Tasks {
        /// Filter by status (e.g., in_progress, next_up)
        #[arg(long)]
        status: Option<String>,
        /// Filter by sprint ID
        #[arg(long)]
        sprint: Option<String>,
    },
    /// List epics
    Epics {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
    },
    /// List sprints
    Sprints {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
    },
    /// Show sync status
    Sync,
}

/// Handle createOS CLI commands.
pub async fn handle_command(cmd: CreateOsCommands, workspace_dir: &std::path::Path) -> Result<()> {
    let db = CreateOsDb::open(workspace_dir)?;

    match cmd {
        CreateOsCommands::Tasks { status, sprint } => {
            let tasks = db.list_tasks(status.as_deref(), sprint.as_deref()).await?;

            if tasks.is_empty() {
                println!("No tasks found.");
                return Ok(());
            }

            println!(
                "{:<8} {:<12} {:<10} {:<50}",
                "ID", "STATUS", "PRIORITY", "TITLE"
            );
            println!("{}", "-".repeat(82));
            for task in &tasks {
                println!(
                    "{:<8} {:<12} {:<10} {:<50}",
                    &task.id.to_string()[..8],
                    task.status,
                    task.priority,
                    truncate(&task.title, 50),
                );
            }
            println!("\n{} task(s)", tasks.len());
        }

        CreateOsCommands::Epics { status } => {
            let reader = db
                .inner()
                .reader()
                .await
                .map_err(|e| anyhow::anyhow!("Reader failed: {e}"))?;

            let epics = reader
                .query(move |conn| {
                    let mut sql =
                        String::from("SELECT id, name, status, priority FROM createos_epic");
                    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                    if let Some(ref s) = status {
                        sql.push_str(" WHERE status = ?");
                        params.push(Box::new(s.clone()));
                    }
                    sql.push_str(" ORDER BY created_at DESC");

                    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                        params.iter().map(|p| p.as_ref()).collect();
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(param_refs.as_slice(), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })?;

                    let mut result = Vec::new();
                    for row in rows {
                        result.push(row?);
                    }
                    Ok(result)
                })
                .map_err(|e| anyhow::anyhow!("Epic query failed: {e}"))?;

            if epics.is_empty() {
                println!("No epics found.");
                return Ok(());
            }

            println!(
                "{:<8} {:<12} {:<10} {:<50}",
                "ID", "STATUS", "PRIORITY", "NAME"
            );
            println!("{}", "-".repeat(82));
            for (id, name, status, priority) in &epics {
                println!(
                    "{:<8} {:<12} {:<10} {:<50}",
                    &id[..8.min(id.len())],
                    status,
                    priority,
                    truncate(name, 50),
                );
            }
            println!("\n{} epic(s)", epics.len());
        }

        CreateOsCommands::Sprints { status } => {
            let reader = db
                .inner()
                .reader()
                .await
                .map_err(|e| anyhow::anyhow!("Reader failed: {e}"))?;

            let sprints = reader
                .query(move |conn| {
                    let mut sql = String::from(
                        "SELECT id, name, status, start_date, end_date FROM createos_sprint",
                    );
                    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                    if let Some(ref s) = status {
                        sql.push_str(" WHERE status = ?");
                        params.push(Box::new(s.clone()));
                    }
                    sql.push_str(" ORDER BY start_date DESC NULLS LAST");

                    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                        params.iter().map(|p| p.as_ref()).collect();
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(param_refs.as_slice(), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    })?;

                    let mut result = Vec::new();
                    for row in rows {
                        result.push(row?);
                    }
                    Ok(result)
                })
                .map_err(|e| anyhow::anyhow!("Sprint query failed: {e}"))?;

            if sprints.is_empty() {
                println!("No sprints found.");
                return Ok(());
            }

            println!(
                "{:<8} {:<12} {:<12} {:<12} {:<40}",
                "ID", "STATUS", "START", "END", "NAME"
            );
            println!("{}", "-".repeat(86));
            for (id, name, status, start, end) in &sprints {
                println!(
                    "{:<8} {:<12} {:<12} {:<12} {:<40}",
                    &id[..8.min(id.len())],
                    status,
                    start.as_deref().unwrap_or("-"),
                    end.as_deref().unwrap_or("-"),
                    truncate(name, 40),
                );
            }
            println!("\n{} sprint(s)", sprints.len());
        }

        CreateOsCommands::Sync => {
            let status = db.sync_status();
            println!("createOS Sync Status");
            println!("  Database: {}", db.db_path().display());
            println!("  Connected: {}", status.connected);
            println!(
                "  Last synced: {}",
                if status.last_synced_at.is_some() {
                    "yes"
                } else {
                    "never"
                }
            );
            println!("  Uploading: {}", status.uploading);
            println!("  Downloading: {}", status.downloading);
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
