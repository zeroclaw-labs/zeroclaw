use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use super::models::{NewTask, TaskUpdate};
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
    /// Run one orchestrator tick (sprint lifecycle + pick next task)
    Orchestrate,
    /// Show orchestrator status: active sprint, task counts, next task
    Status,
    /// Create a new task
    CreateTask {
        /// Task title
        #[arg(long)]
        title: String,
        /// Task description
        #[arg(long, default_value = "")]
        description: String,
        /// Priority: p1_urgent, p2_high, p3_medium, p4_low
        #[arg(long, default_value = "p3_medium")]
        priority: String,
        /// Task type: feature, fix, hotfix, chore, docs
        #[arg(long, default_value = "chore")]
        task_type: String,
        /// Task category: software_dev, general, etc.
        #[arg(long, default_value = "general")]
        task_category: String,
        /// Sprint ID to assign to
        #[arg(long)]
        sprint: Option<String>,
        /// Epic ID
        #[arg(long)]
        epic: Option<String>,
        /// Domain ID
        #[arg(long)]
        domain: Option<String>,
    },
    /// Update an existing task
    UpdateTask {
        /// Task ID (prefix match supported)
        id: String,
        /// New status
        #[arg(long)]
        status: Option<String>,
        /// New priority
        #[arg(long)]
        priority: Option<String>,
        /// New title
        #[arg(long)]
        title: Option<String>,
        /// Assign to sprint
        #[arg(long)]
        sprint: Option<String>,
        /// Set note
        #[arg(long)]
        note: Option<String>,
    },
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

            let mut sql = String::from("SELECT id, name, status, priority FROM createos_epic");
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(ref s) = status {
                sql.push_str(" WHERE status = ?");
                params.push(Box::new(s.clone()));
            }
            sql.push_str(" ORDER BY created_at DESC");

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = reader
                .prepare(&sql)
                .map_err(|e| anyhow::anyhow!("Prepare failed: {e}"))?;
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(|e| anyhow::anyhow!("Epic query failed: {e}"))?;

            let mut epics = Vec::new();
            for row in rows {
                epics.push(row.map_err(|e| anyhow::anyhow!("Row parse error: {e}"))?);
            }

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

            let mut sql =
                String::from("SELECT id, name, status, start_date, end_date FROM createos_sprint");
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(ref s) = status {
                sql.push_str(" WHERE status = ?");
                params.push(Box::new(s.clone()));
            }
            sql.push_str(" ORDER BY start_date DESC NULLS LAST");

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = reader
                .prepare(&sql)
                .map_err(|e| anyhow::anyhow!("Prepare failed: {e}"))?;
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(|e| anyhow::anyhow!("Sprint query failed: {e}"))?;

            let mut sprints = Vec::new();
            for row in rows {
                sprints.push(row.map_err(|e| anyhow::anyhow!("Row parse error: {e}"))?);
            }

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
            println!("  Connected: {}", status.is_connected());
            println!("  Connecting: {}", status.is_connecting());
            println!("  Uploading: {}", status.is_uploading());
            println!("  Downloading: {}", status.is_downloading());
            if let Some(err) = status.download_error() {
                println!("  Download error: {err}");
            }
            if let Some(err) = status.upload_error() {
                println!("  Upload error: {err}");
            }
        }

        CreateOsCommands::Orchestrate => {
            let result = super::orchestrator::run_once(&db).await?;

            if !result.sprints_completed.is_empty() {
                for id in &result.sprints_completed {
                    println!("Sprint completed: {}", &id[..8.min(id.len())]);
                }
            }
            if let Some(ref id) = result.sprint_activated {
                println!("Sprint activated: {}", &id[..8.min(id.len())]);
            }
            if let Some(ref id) = result.sprint_auto_planned {
                println!("Auto-planned sprint: {}", &id[..8.min(id.len())]);
            }
            if let Some(ref title) = result.next_task_title {
                let id = result.next_task_id.as_deref().unwrap_or("?");
                println!("Next task: [{}] {}", &id[..8.min(id.len())], title);
            } else {
                println!("No approved tasks queued.");
            }
        }

        CreateOsCommands::Status => {
            // Active sprint info
            let active = db.list_sprints(Some("active")).await?;
            if let Some(sprint) = active.first() {
                let (done, total) = db.count_sprint_tasks(&sprint.id.to_string()).await?;
                println!(
                    "Active Sprint: {} [{}]",
                    sprint.name,
                    &sprint.id.to_string()[..8]
                );
                println!("  Progress: {done}/{total} tasks done");
                if let Some(ref start) = sprint.start_date {
                    println!("  Started: {start}");
                }
                if let Some(ref end) = sprint.end_date {
                    println!("  Target: {end}");
                }
            } else {
                println!("No active sprint.");
            }

            // Planned sprints
            let planned = db.list_sprints(Some("not_started")).await?;
            if !planned.is_empty() {
                println!("\n{} planned sprint(s) queued", planned.len());
            }

            // Next task
            if let Some(task) = db.pick_next_task().await? {
                println!(
                    "\nNext task: [{}] {} ({})",
                    &task.id.to_string()[..8],
                    task.title,
                    task.priority,
                );
            } else {
                println!("\nNo approved tasks queued.");
            }

            // Task counts by status
            let all_tasks = db.list_tasks(None, None).await?;
            let mut counts = std::collections::HashMap::new();
            for t in &all_tasks {
                *counts.entry(t.status.as_str()).or_insert(0usize) += 1;
            }
            if !counts.is_empty() {
                println!("\nTask breakdown:");
                let mut sorted: Vec<_> = counts.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));
                for (status, count) in sorted {
                    println!("  {status}: {count}");
                }
            }
        }

        CreateOsCommands::CreateTask {
            title,
            description,
            priority,
            task_type,
            task_category,
            sprint,
            epic,
            domain,
        } => {
            let task = db
                .create_task(NewTask {
                    title,
                    description,
                    acceptance_criteria: String::new(),
                    domain_id: domain,
                    epic_id: epic,
                    sprint_id: sprint,
                    user_story_id: None,
                    status: "approved".to_string(),
                    priority,
                    task_type,
                    task_category,
                    due_date: None,
                })
                .await?;
            println!(
                "Created task: [{}] {}",
                &task.id.to_string()[..8],
                task.title
            );
        }

        CreateOsCommands::UpdateTask {
            id,
            status,
            priority,
            title,
            sprint,
            note,
        } => {
            let updates = TaskUpdate {
                status,
                priority,
                title,
                sprint_id: sprint,
                note,
                ..Default::default()
            };
            db.update_task(&id, updates).await?;
            println!("Updated task {}", &id[..8.min(id.len())]);
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
