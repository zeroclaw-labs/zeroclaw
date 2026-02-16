use crate::config::Config;
use crate::memory;
use crate::providers;
use crate::security::SecurityPolicy;
use crate::status_events;
use anyhow::Result;
use chrono::Utc;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio::time::Duration;

const STATUS_FLUSH_SECONDS: u64 = 5;

fn install_status_persist_hook(event_db: crate::aria::db::AriaDb, tenant_fallback: String) {
    crate::status_events::set_persist_hook(std::sync::Arc::new(
        move |event_type, data, timestamp| {
            if let Err(e) = crate::dashboard::persist_status_event(
                &event_db,
                &tenant_fallback,
                event_type,
                data,
                timestamp,
            ) {
                tracing::warn!("Failed to persist status event '{event_type}': {e}");
            }
            match crate::dashboard::maybe_create_inbox_for_status_event(
                &event_db,
                &tenant_fallback,
                event_type,
                data,
            ) {
                Ok(Some(created)) => {
                    crate::status_events::emit(
                        "inbox.item.created",
                        serde_json::json!({
                            "tenantId": created.tenant_id,
                            "id": created.id,
                            "title": created.title,
                            "sourceType": created.source_type,
                        }),
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        "Failed to persist inbox item for status event '{event_type}': {e}"
                    );
                }
            }
        },
    ));
}

pub async fn run(config: Config, host: String, port: u16) -> Result<()> {
    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    if let Ok(event_db) = crate::aria::db::AriaDb::open(&config.registry_db_path()) {
        install_status_persist_hook(event_db, "dev-tenant".to_string());
    }

    if let Err(e) = crate::cron::jobs_file::import_jobs_file(&config.workspace_dir) {
        tracing::warn!("Failed to import ~/.aria/jobs.json before daemon start: {e}");
    }

    wire_cron_bridge_hooks(&config)?;

    if config.heartbeat.enabled {
        let _ =
            crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&config.workspace_dir)
                .await;
    }

    let mut handles: Vec<JoinHandle<()>> = vec![spawn_state_writer(config.clone())];

    {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        handles.push(spawn_component_supervisor(
            "gateway",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = gateway_cfg.clone();
                let host = gateway_host.clone();
                async move { crate::gateway::run_gateway(&host, port, cfg).await }
            },
        ));
    }

    {
        if has_supervised_channels(&config) {
            let channels_cfg = config.clone();
            handles.push(spawn_component_supervisor(
                "channels",
                initial_backoff,
                max_backoff,
                move || {
                    let cfg = channels_cfg.clone();
                    async move { crate::channels::start_channels(cfg).await }
                },
            ));
        } else {
            crate::health::mark_component_ok("channels");
            tracing::info!("No real-time channels configured; channel supervisor disabled");
        }
    }

    if config.heartbeat.enabled {
        let heartbeat_cfg = config.clone();
        handles.push(spawn_component_supervisor(
            "heartbeat",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = heartbeat_cfg.clone();
                async move { run_heartbeat_worker(cfg).await }
            },
        ));
    }

    {
        let scheduler_cfg = config.clone();
        handles.push(spawn_component_supervisor(
            "scheduler",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = scheduler_cfg.clone();
                async move { crate::cron::scheduler::run(cfg).await }
            },
        ));
    }

    {
        let feed_cfg = config.clone();
        handles.push(spawn_component_supervisor(
            "feed-scheduler",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = feed_cfg.clone();
                async move { run_feed_scheduler_worker(cfg).await }
            },
        ));
    }

    println!("🧠 Aria daemon started");
    println!("   Gateway:  http://{host}:{port}");
    println!("   Components: gateway, channels, heartbeat, scheduler, feed-scheduler");
    println!("   Ctrl+C to stop");

    tokio::signal::ctrl_c().await?;
    crate::health::mark_component_error("daemon", "shutdown requested");

    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
    }
    crate::status_events::clear_persist_hook();

    Ok(())
}

pub fn state_file_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("daemon_state.json")
}

fn spawn_state_writer(config: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = state_file_path(&config);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let mut interval = tokio::time::interval(Duration::from_secs(STATUS_FLUSH_SECONDS));
        loop {
            interval.tick().await;
            let mut json = crate::health::snapshot_json();
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "written_at".into(),
                    serde_json::json!(Utc::now().to_rfc3339()),
                );
            }
            let data = serde_json::to_vec_pretty(&json).unwrap_or_else(|_| b"{}".to_vec());
            let _ = tokio::fs::write(&path, data).await;
        }
    })
}

fn spawn_component_supervisor<F, Fut>(
    name: &'static str,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    mut run_component: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = initial_backoff_secs.max(1);
        let max_backoff = max_backoff_secs.max(backoff);

        loop {
            crate::health::mark_component_ok(name);
            match run_component().await {
                Ok(()) => {
                    crate::health::mark_component_error(name, "component exited unexpectedly");
                    tracing::warn!("Daemon component '{name}' exited unexpectedly");
                }
                Err(e) => {
                    crate::health::mark_component_error(name, e.to_string());
                    tracing::error!("Daemon component '{name}' failed: {e}");
                }
            }

            crate::health::bump_component_restart(name);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = backoff.saturating_mul(2).min(max_backoff);
        }
    })
}

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = crate::heartbeat::engine::HeartbeatEngine::new(
        config.heartbeat.clone(),
        config.workspace_dir.clone(),
        observer,
    );

    let interval_mins = config.heartbeat.interval_minutes.max(5);
    let mut interval = tokio::time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

    loop {
        interval.tick().await;
        status_events::emit(
            "heartbeat.tick",
            serde_json::json!({
                "tenantId": "dev-tenant",
                "intervalMinutes": interval_mins,
            }),
        );

        let tasks = engine.collect_tasks_for_tick().await?;
        if tasks.is_empty() {
            continue;
        }

        for task in tasks {
            let prompt = format!("[Heartbeat Task] {task}");
            match execute_heartbeat_task(&config, &prompt).await {
                Ok(output) => {
                    crate::health::mark_component_ok("heartbeat");
                    let mut deduped = false;
                    if let Err(e) = persist_heartbeat_response_to_inbox(&config, &task, &output)
                        .map(|created| {
                            deduped = !created;
                        })
                    {
                        tracing::warn!("Failed to persist heartbeat response: {e}");
                    }
                    status_events::emit(
                        "heartbeat.task.completed",
                        serde_json::json!({
                            "tenantId": "dev-tenant",
                            "task": task,
                            "summary": output.lines().next().unwrap_or("completed"),
                            "deduped": deduped,
                        }),
                    );
                }
                Err(e) => {
                    crate::health::mark_component_error("heartbeat", e.to_string());
                    tracing::warn!("Heartbeat task failed: {e}");
                    status_events::emit(
                        "heartbeat.task.failed",
                        serde_json::json!({
                            "tenantId": "dev-tenant",
                            "task": task,
                            "error": e.to_string(),
                        }),
                    );
                }
            }
        }
    }
}

async fn execute_heartbeat_task(config: &Config, prompt: &str) -> Result<String> {
    let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
    let model_name = config
        .default_model
        .as_deref()
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider = providers::create_resilient_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;
    let mem = memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let registry_db = crate::aria::db::AriaDb::open(&config.registry_db_path())?;
    let tenant = crate::tenant::resolve_tenant_from_token(&registry_db, "");

    let result = crate::agent::orchestrator::run_live_turn(
        crate::agent::orchestrator::LiveTurnConfig {
            provider: provider.as_ref(),
            security: &security,
            memory: Arc::from(mem),
            composio_api_key: if config.composio.enabled {
                config.composio.api_key.as_deref()
            } else {
                None
            },
            browser_config: &config.browser,
            registry_db: &registry_db,
            workspace_dir: &config.workspace_dir,
            tenant_id: &tenant,
            model: model_name,
            temperature: config.default_temperature,
            mode_hint: "heartbeat",
            max_turns: Some(25),
            external_tool_context: None,
        },
        prompt,
        None,
    )
    .await?;

    Ok(result.output)
}

const HEARTBEAT_INBOX_DEDUP_WINDOW_MINUTES: i64 = 120;

fn persist_heartbeat_response_to_inbox(config: &Config, task: &str, output: &str) -> Result<bool> {
    let db = crate::aria::db::AriaDb::open(&config.registry_db_path())?;
    crate::dashboard::ensure_schema(&db)?;
    let tenant = crate::tenant::resolve_tenant_from_token(&db, "");
    let ts = chrono::Utc::now().timestamp_millis();
    let normalized_task = task.trim().to_lowercase();
    let source_id = format!("heartbeat:{normalized_task}");
    let dedup_cutoff = ts - (HEARTBEAT_INBOX_DEDUP_WINDOW_MINUTES * 60_i64 * 1_000_i64);
    let preview = output.lines().next().unwrap_or("Heartbeat task completed");
    let preview_limited = preview.chars().take(160).collect::<String>();
    let metadata = serde_json::json!({
        "kind": "heartbeat-response",
        "task": task,
        "dedupWindowMinutes": HEARTBEAT_INBOX_DEDUP_WINDOW_MINUTES,
        "createdAtMs": ts,
    });
    let metadata_json = serde_json::to_string(&metadata)?;

    let existing_id = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id FROM inbox_items
             WHERE tenant_id=?1
               AND source_type='system'
               AND source_id=?2
               AND status!='archived'
             ORDER BY created_at DESC
             LIMIT 1",
        )?;
        let row = stmt.query_row(rusqlite::params![tenant, source_id], |r| {
            r.get::<_, String>(0)
        });
        match row {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })?;

    if let Some(existing_id) = existing_id {
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE inbox_items
                 SET title=?1,
                     preview=?2,
                     body=?3,
                     metadata_json=?4,
                     status='unread',
                     read_at=NULL,
                     created_at=?5
                 WHERE tenant_id=?6
                   AND id=?7",
                rusqlite::params![
                    format!("Heartbeat: {}", task.trim()),
                    preview_limited,
                    output.trim(),
                    metadata_json,
                    ts,
                    tenant,
                    existing_id
                ],
            )?;
            Ok(())
        })?;
        return Ok(false);
    }

    let recent_count = db.with_conn(|conn| {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM inbox_items
             WHERE tenant_id=?1
               AND source_type='system'
               AND source_id=?2
               AND created_at>=?3",
            rusqlite::params![tenant, source_id, dedup_cutoff],
            |row| row.get(0),
        )?;
        Ok(count)
    })?;
    if recent_count > 0 {
        return Ok(false);
    }

    let item = crate::dashboard::NewInboxItem {
        source_type: "system".to_string(),
        source_id: Some(source_id),
        run_id: None,
        chat_id: None,
        title: format!("Heartbeat: {}", task.trim()),
        preview: Some(preview_limited),
        body: Some(output.trim().to_string()),
        metadata,
        status: Some("unread".to_string()),
    };
    crate::dashboard::create_inbox_item(&db, &tenant, &item)?;
    Ok(true)
}

fn wire_cron_bridge_hooks(config: &Config) -> Result<()> {
    let aria_db = crate::aria::db::AriaDb::open(&config.registry_db_path())?;
    let add_cfg = config.clone();
    let remove_cfg = config.clone();
    let export_cfg = config.clone();

    let add_job: crate::aria::cron_bridge::AddJobFn = Arc::new(move |expr, timezone, command| {
        let job = crate::cron::add_job(&add_cfg, expr, timezone, command)?;
        Ok(crate::aria::cron_bridge::CronJobHandle { id: job.id })
    });
    let remove_job: crate::aria::cron_bridge::RemoveJobFn =
        Arc::new(move |job_id| crate::cron::remove_job(&remove_cfg, job_id));

    let bridge = Arc::new(crate::aria::cron_bridge::CronBridge::new(
        aria_db, add_job, remove_job,
    ));
    bridge.sync_all()?;
    if let Err(e) = crate::cron::jobs_file::export_jobs_file(&export_cfg.workspace_dir) {
        tracing::warn!("Failed to export cron jobs to ~/.aria/jobs.json after startup sync: {e}");
    }

    let on_uploaded = bridge.clone();
    let on_deleted = bridge.clone();
    let export_on_upload = config.workspace_dir.clone();
    let export_on_delete = config.workspace_dir.clone();
    crate::aria::hooks::set_cron_hooks(crate::aria::hooks::CronHooks {
        on_cron_uploaded: Some(Box::new(move |cron_id| {
            if let Err(e) = on_uploaded.sync_cron(cron_id) {
                tracing::warn!("Failed to sync cron '{cron_id}' after upload: {e}");
            } else if let Err(e) = crate::cron::jobs_file::export_jobs_file(&export_on_upload) {
                tracing::warn!("Failed to export cron jobs after upload hook: {e}");
            }
        })),
        on_cron_deleted: Some(Box::new(move |cron_id| {
            if let Err(e) = on_deleted.remove_cron(cron_id) {
                tracing::warn!("Failed to remove cron '{cron_id}' after delete: {e}");
            } else if let Err(e) = crate::cron::jobs_file::export_jobs_file(&export_on_delete) {
                tracing::warn!("Failed to export cron jobs after delete hook: {e}");
            }
        })),
    });

    Ok(())
}

async fn run_feed_scheduler_worker(config: Config) -> Result<()> {
    let db = crate::aria::db::AriaDb::open(&config.registry_db_path())?;
    let scheduler = crate::feed::scheduler::FeedScheduler::new(
        db.clone(),
        crate::feed::executor::FeedExecutor::new(db),
    );
    scheduler.start().await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, bool)>();
    crate::aria::hooks::set_feed_hooks(crate::aria::hooks::FeedHooks {
        on_feed_uploaded: Some(Box::new({
            let tx = tx.clone();
            move |feed_id| {
                let _ = tx.send((feed_id.to_string(), true));
            }
        })),
        on_feed_deleted: Some(Box::new(move |feed_id| {
            let _ = tx.send((feed_id.to_string(), false));
        })),
    });

    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            maybe_evt = rx.recv() => {
                if let Some((feed_id, uploaded)) = maybe_evt {
                    if uploaded {
                        scheduler.sync_feed(&feed_id).await?;
                    } else {
                        scheduler.remove_feed(&feed_id);
                    }
                }
            }
            _ = heartbeat.tick() => {
                crate::health::mark_component_ok("feed-scheduler");
            }
        }
    }
}

fn has_supervised_channels(config: &Config) -> bool {
    config.channels_config.telegram.is_some()
        || config.channels_config.discord.is_some()
        || config.channels_config.slack.is_some()
        || config.channels_config.imessage.is_some()
        || config.channels_config.matrix.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config::default();
        config.workspace_dir = tmp.path().join("workspace");
        config.config_path = tmp.path().join("config.toml");
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn state_file_path_uses_config_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let path = state_file_path(&config);
        assert_eq!(path, tmp.path().join("daemon_state.json"));
    }

    #[tokio::test]
    async fn supervisor_marks_error_and_restart_on_failure() {
        let handle = spawn_component_supervisor("daemon-test-fail", 1, 1, || async {
            anyhow::bail!("boom")
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        let _ = handle.await;

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["daemon-test-fail"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("boom"));
    }

    #[tokio::test]
    async fn supervisor_marks_unexpected_exit_as_error() {
        let handle = spawn_component_supervisor("daemon-test-exit", 1, 1, || async { Ok(()) });

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        let _ = handle.await;

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["daemon-test-exit"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("component exited unexpectedly"));
    }

    #[test]
    fn detects_no_supervised_channels() {
        let config = Config::default();
        assert!(!has_supervised_channels(&config));
    }

    #[test]
    fn detects_supervised_channels_present() {
        let mut config = Config::default();
        config.channels_config.telegram = Some(crate::config::TelegramConfig {
            bot_token: "token".into(),
            allowed_users: vec![],
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn subagent_failed_persists_event_and_creates_inbox_with_broadcast() {
        let db = crate::aria::db::AriaDb::open_in_memory().unwrap();
        crate::dashboard::ensure_schema(&db).unwrap();
        install_status_persist_hook(db.clone(), "dev-tenant".to_string());

        let (sub_id, mut rx) = crate::status_events::subscribe();
        crate::status_events::emit(
            "subagent.failed",
            serde_json::json!({
                "tenantId": "dev-tenant",
                "taskLabel": "Contract probe",
                "error": "forced failure",
                "runId": "run-test",
                "chatId": "chat-test",
                "toolId": "tool-test"
            }),
        );

        let mut saw_inbox_created = false;
        for _ in 0..4 {
            if let Ok(json) = rx.try_recv() {
                if json.contains("\"type\":\"inbox.item.created\"") {
                    saw_inbox_created = true;
                    break;
                }
            }
        }

        let counts = db
            .with_conn(|conn| {
                let events_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM events WHERE type='subagent.failed'",
                    [],
                    |r| r.get(0),
                )?;
                let inbox_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM inbox_items WHERE source_type='subagent'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((events_count, inbox_count))
            })
            .unwrap();

        crate::status_events::unsubscribe(sub_id);
        crate::status_events::clear_persist_hook();

        assert_eq!(counts.0, 1);
        assert_eq!(counts.1, 1);
        assert!(saw_inbox_created);
    }

    #[test]
    fn heartbeat_inbox_persistence_upserts_existing_item() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let created =
            persist_heartbeat_response_to_inbox(&config, "Check weather", "First output").unwrap();
        assert!(created);

        let db = crate::aria::db::AriaDb::open(&config.registry_db_path()).unwrap();
        let (count_after_first, first_body): (i64, String) = db
            .with_conn(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0))?;
                let body: String =
                    conn.query_row("SELECT body FROM inbox_items LIMIT 1", [], |r| r.get(0))?;
                Ok((count, body))
            })
            .unwrap();
        assert_eq!(count_after_first, 1);
        assert_eq!(first_body, "First output");

        let created_second =
            persist_heartbeat_response_to_inbox(&config, "Check weather", "Second output").unwrap();
        assert!(!created_second);

        let (count_after_second, second_body, status): (i64, String, String) = db
            .with_conn(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0))?;
                let row =
                    conn.query_row("SELECT body, status FROM inbox_items LIMIT 1", [], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?;
                Ok((count, row.0, row.1))
            })
            .unwrap();
        assert_eq!(count_after_second, 1);
        assert_eq!(second_body, "Second output");
        assert_eq!(status, "unread");
    }
}
