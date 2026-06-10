use anyhow::Result;
use chrono::Utc;
use std::future::Future;
use std::path::PathBuf;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use daemonclaw_config::schema::Config;

const STATUS_FLUSH_SECONDS: u64 = 60;
const MAX_HEALTH_ROWS: u64 = 10_000;

/// Wait for shutdown signal (SIGINT or SIGTERM).
/// SIGHUP is explicitly ignored so the daemon survives terminal/SSH disconnects.
async fn wait_for_shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sighup = signal(SignalKind::hangup())?;

        loop {
            tokio::select! {
                _ = sigint.recv() => {
                    tracing::info!("Received SIGINT, shutting down...");
                    break;
                }
                _ = sigterm.recv() => {
                    tracing::info!("Received SIGTERM, shutting down...");
                    break;
                }
                _ = sighup.recv() => {
                    tracing::info!("Received SIGHUP, ignoring (daemon stays running)");
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        tracing::info!("Received Ctrl+C, shutting down...");
    }

    Ok(())
}

/// Optional subsystem start functions injected by the binary crate.
/// This allows the daemon to spawn subsystems without depending on their crates.
#[allow(clippy::type_complexity)]
pub struct DaemonSubsystems {
    /// Start the gateway HTTP server. Injected by the binary when `gateway` feature is on.
    pub gateway_start: Option<
        Box<
            dyn Fn(
                    String,
                    u16,
                    Config,
                    Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
                ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>
                + Send
                + Sync,
        >,
    >,
    /// Start supervised channels. Injected by the binary when channels crate is available.
    pub channels_start: Option<
        Box<
            dyn Fn(Config) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>
                + Send
                + Sync,
        >,
    >,
    /// Start the MQTT SOP listener. Injected by the binary when channels crate is available.
    pub mqtt_start: Option<
        Box<
            dyn Fn(
                    daemonclaw_config::schema::MqttConfig,
                ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>>
                + Send
                + Sync,
        >,
    >,
}

pub async fn run(
    config: Config,
    host: String,
    port: u16,
    subsystems: DaemonSubsystems,
) -> Result<()> {
    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    // Shared broadcast channel so all daemon components (gateway, cron,
    // heartbeat) can publish real-time events to dashboard clients.
    let (event_tx, _rx) = tokio::sync::broadcast::channel::<serde_json::Value>(256);

    if config.heartbeat.enabled {
        let _ =
            crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&config.workspace_dir)
                .await;
    }

    let mut handles: Vec<JoinHandle<()>> = vec![spawn_state_writer(config.clone())];

    if let Some(gateway_start) = subsystems.gateway_start {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        let gateway_event_tx = event_tx.clone();
        let gateway_start = std::sync::Arc::new(gateway_start);
        handles.push(spawn_component_supervisor(
            "gateway",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = gateway_cfg.clone();
                let host = gateway_host.clone();
                let tx = gateway_event_tx.clone();
                let start = gateway_start.clone();
                async move { start(host, port, cfg, Some(tx)).await }
            },
        ));
    }

    if let Some(channels_start) = subsystems.channels_start {
        if has_supervised_channels(&config) {
            let channels_cfg = config.clone();
            let channels_start = std::sync::Arc::new(channels_start);
            handles.push(spawn_component_supervisor(
                "channels",
                initial_backoff,
                max_backoff,
                move || {
                    let cfg = channels_cfg.clone();
                    let start = channels_start.clone();
                    async move { start(cfg).await }
                },
            ));
        } else {
            crate::health::mark_component_ok("channels");
            tracing::info!("No channels configured; channel supervisor disabled");
        }
    } else {
        crate::health::mark_component_ok("channels");
        tracing::info!("Channels subsystem not wired; channel supervisor disabled");
    }

    // Wire up MQTT SOP listener if configured and enabled
    if let Some(mqtt_start) = subsystems.mqtt_start {
        if let Some(ref mqtt_config) = config.channels.mqtt {
            if mqtt_config.enabled {
                let mqtt_cfg = mqtt_config.clone();
                let mqtt_start = std::sync::Arc::new(mqtt_start);
                handles.push(spawn_component_supervisor(
                    "mqtt",
                    initial_backoff,
                    max_backoff,
                    move || {
                        let cfg = mqtt_cfg.clone();
                        let start = mqtt_start.clone();
                        async move { start(cfg).await }
                    },
                ));
            } else {
                tracing::info!("MQTT channel configured but disabled (enabled = false)");
                crate::health::mark_component_ok("mqtt");
            }
        } else {
            crate::health::mark_component_ok("mqtt");
        }
    } else {
        crate::health::mark_component_ok("mqtt");
    }

    if config.heartbeat.enabled {
        let heartbeat_cfg = config.clone();
        handles.push(spawn_component_supervisor(
            "heartbeat",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = heartbeat_cfg.clone();
                async move { Box::pin(run_heartbeat_worker(cfg)).await }
            },
        ));
    }

    if config.cron.enabled {
        let scheduler_cfg = config.clone();
        let scheduler_event_tx = event_tx.clone();
        handles.push(spawn_component_supervisor(
            "scheduler",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = scheduler_cfg.clone();
                let tx = scheduler_event_tx.clone();
                async move { Box::pin(crate::cron::scheduler::run(cfg, Some(tx))).await }
            },
        ));
    } else {
        crate::health::mark_component_ok("scheduler");
        tracing::info!("Cron disabled; scheduler supervisor not started");
    }

    crate::health::touch_liveness(&config.workspace_dir);

    println!("🧠 DaemonClaw daemon started");
    println!("   Gateway:  http://{host}:{port}");
    println!("   Components: gateway, channels, heartbeat, scheduler");
    if config.gateway.require_pairing {
        println!("   Pairing:    enabled (code appears in gateway output above)");
    }
    println!("   Ctrl+C or SIGTERM to stop");

    // Wait for shutdown signal (SIGINT or SIGTERM)
    wait_for_shutdown_signal().await?;
    crate::health::mark_component_error("daemon", "shutdown requested");

    // Graceful shutdown: write final state before aborting subsystems.
    // TimeoutStopSec=30s in the service unit gives us a hard deadline.
    let shutdown_deadline = tokio::time::Instant::now() + Duration::from_secs(25);

    // 1. Final health snapshot to state.db
    write_health_snapshot_to_db(&config, Some("clean"));

    // 2. Touch liveness so the watchdog timer doesn't restart us during the stop window
    crate::health::touch_liveness(&config.workspace_dir);

    // 3. Abort subsystems and wait up to the deadline for them to finish
    for handle in &handles {
        handle.abort();
    }
    let _ = tokio::time::timeout_at(shutdown_deadline, async {
        for handle in handles {
            let _ = handle.await;
        }
    })
    .await;

    tracing::info!("Graceful shutdown complete");
    Ok(())
}

pub fn state_file_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("daemon_state.json")
}

fn write_health_snapshot_to_db(config: &Config, shutdown: Option<&str>) {
    let json = crate::health::snapshot_json();
    let written_at = Utc::now().to_rfc3339();
    let pid = json.get("pid").and_then(|v| v.as_i64());
    let uptime = json.get("uptime_seconds").and_then(|v| v.as_i64());
    let components = json
        .get("components")
        .map(|c| c.to_string())
        .unwrap_or_else(|| "{}".to_string());

    let Ok(state_db) = daemonclaw_config::state_db::StateDb::open(&config.workspace_dir) else {
        return;
    };
    let _ = state_db.ensure_daemon_health_table();
    let Ok(conn) = state_db.connect() else { return };
    let _ = conn.execute(
        "INSERT INTO daemon_health (written_at, pid, uptime_secs, shutdown, components) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![written_at, pid, uptime, shutdown, components],
    );
    let _ = conn.execute(
        "DELETE FROM daemon_health WHERE id NOT IN (SELECT id FROM daemon_health ORDER BY id DESC LIMIT ?1)",
        rusqlite::params![MAX_HEALTH_ROWS],
    );
}

fn spawn_state_writer(config: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(STATUS_FLUSH_SECONDS));
        loop {
            interval.tick().await;
            let cfg = config.clone();
            let _ = tokio::task::spawn_blocking(move || {
                write_health_snapshot_to_db(&cfg, None);
            })
            .await;
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
                    // Clean exit — reset backoff since the component ran successfully
                    backoff = initial_backoff_secs.max(1);
                }
                Err(e) => {
                    crate::health::mark_component_error(name, e.to_string());
                    tracing::error!("Daemon component '{name}' failed: {e}");
                }
            }

            crate::health::bump_component_restart(name);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            // Double backoff AFTER sleeping so first error uses initial_backoff
            backoff = backoff.saturating_mul(2).min(max_backoff);
        }
    })
}

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    use crate::heartbeat::engine::compute_adaptive_interval;
    use std::sync::Arc;

    let metrics = Arc::new(parking_lot::Mutex::new(
        crate::heartbeat::engine::HeartbeatMetrics::default(),
    ));
    let delivery = resolve_heartbeat_delivery(&config)?;
    let adaptive = config.heartbeat.adaptive;
    let autonomous_pickup = config.heartbeat.autonomous_pickup;
    let start_time = std::time::Instant::now();

    // ── Deadman watcher ──────────────────────────────────────────
    let deadman_timeout = config.heartbeat.deadman_timeout_minutes;
    if deadman_timeout > 0 {
        let dm_metrics = Arc::clone(&metrics);
        let dm_config = config.clone();
        let dm_delivery = delivery.clone();
        tokio::spawn(async move {
            let check_interval = Duration::from_secs(60);
            let timeout = chrono::Duration::minutes(i64::from(deadman_timeout));
            loop {
                tokio::time::sleep(check_interval).await;
                let last_tick = dm_metrics.lock().last_tick_at;
                if let Some(last) = last_tick
                    && chrono::Utc::now() - last > timeout
                {
                    let alert = format!(
                        "⚠️ Heartbeat dead-man's switch: no tick in {deadman_timeout} minutes"
                    );
                    let (channel, target) = if let Some(ch) = &dm_config.heartbeat.deadman_channel {
                        let to = dm_config
                            .heartbeat
                            .deadman_to
                            .as_deref()
                            .or(dm_config.heartbeat.to.as_deref())
                            .unwrap_or_default();
                        (ch.clone(), to.to_string())
                    } else if let Some((ch, to)) = &dm_delivery {
                        (ch.clone(), to.clone())
                    } else {
                        continue;
                    };
                    let delivery_fut = crate::cron::scheduler::deliver_announcement(
                        &dm_config, &channel, &target, &alert,
                    );
                    match tokio::time::timeout(Duration::from_secs(30), delivery_fut).await {
                        Ok(Err(e)) => {
                            tracing::warn!("Deadman alert delivery failed: {e}");
                        }
                        Err(_) => {
                            tracing::warn!("Deadman alert delivery timed out (30s)");
                        }
                        Ok(Ok(())) => {}
                    }
                }
            }
        });
    }

    let base_interval = config.heartbeat.interval_minutes.max(1);
    let mut sleep_mins = base_interval;

    loop {
        tokio::time::sleep(Duration::from_secs(u64::from(sleep_mins) * 60)).await;

        // Update uptime
        {
            let mut m = metrics.lock();
            m.uptime_secs = start_time.elapsed().as_secs();
        }

        let tick_start = std::time::Instant::now();

        // ── All modes go through run_task_pickup_tick ────────────
        let (tick_had_error, has_high_priority) =
            run_task_pickup_tick(&config, autonomous_pickup, &delivery).await;

        #[allow(clippy::cast_precision_loss)]
        let tick_elapsed = tick_start.elapsed().as_millis() as f64;
        {
            let mut m = metrics.lock();
            if tick_had_error {
                m.record_failure(tick_elapsed);
            } else {
                m.record_success(tick_elapsed);
            }
        }
        if adaptive {
            let failures = metrics.lock().consecutive_failures;
            sleep_mins = compute_adaptive_interval(
                base_interval,
                config.heartbeat.min_interval_minutes,
                config.heartbeat.max_interval_minutes,
                failures,
                has_high_priority,
            );
        } else {
            sleep_mins = base_interval;
        }
    }
}

/// Run one task-pickup tick: query tasks.db for open tasks eligible for autonomous
/// execution, claim via CAS, execute under task binding.
///
/// - `none`: silent recon — query tasks, log at debug, claim nothing, no LLM calls.
/// - `assisted`: claims and works tasks with `autonomy == Assisted` or `Auto`.
/// - `full`: claims and works tasks with `autonomy == Auto` only.
///
/// Gated tasks (`autonomy == Gated`) are never auto-claimed by any mode.
/// Deterministic tasks (`execution == Deterministic`) are skipped (no runner until G1).
///
/// No channel output except `notify_on_block` (added in Phase 4).
/// No auto-submit — only the agent's `task_submit` tool moves a task to review.
///
/// Returns `(had_error, has_high_priority)` for adaptive interval computation.
async fn run_task_pickup_tick(
    config: &Config,
    pickup: daemonclaw_config::schema::AutonomousPickup,
    delivery: &Option<(String, String)>,
) -> (bool, bool) {
    use crate::tasks::{self, Autonomy, Execution, TaskActor, TaskStatus};

    let workspace = &config.workspace_dir;

    // Touch liveness at tick start
    crate::health::touch_liveness(workspace);

    // ── Recon: find open tasks ──────────────────────────────────
    let open_tasks = match tasks::store::list_tasks(workspace, Some(TaskStatus::Open), 50) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("Heartbeat task-pickup: failed to list tasks: {e}");
            return (true, false);
        }
    };

    let open_count = open_tasks.len();

    // Filter: skip deterministic tasks (no runner until G1).
    // Filter: gated tasks are never auto-claimed.
    let eligible: Vec<_> = open_tasks
        .into_iter()
        .filter(|t| t.execution != Execution::Deterministic)
        .filter(|t| match pickup {
            daemonclaw_config::schema::AutonomousPickup::Full => t.autonomy == Autonomy::Auto,
            daemonclaw_config::schema::AutonomousPickup::Assisted => {
                t.autonomy == Autonomy::Auto || t.autonomy == Autonomy::Assisted
            }
            // none mode: recon only — nothing is eligible for claiming
            daemonclaw_config::schema::AutonomousPickup::None => false,
        })
        .collect();

    let has_high_priority = eligible.iter().any(|t| t.priority >= 3);

    // ── None mode: silent recon, no LLM, no claims ──────────────
    if pickup == daemonclaw_config::schema::AutonomousPickup::None {
        tracing::debug!(
            "Heartbeat recon: {} open tasks (none mode — no claims)",
            open_count,
        );
        return (false, has_high_priority);
    }

    // ── Continue-my-own-active: resume tasks assigned to heartbeat ──
    let active_tasks = match tasks::store::list_tasks(workspace, Some(TaskStatus::Active), 50) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("Heartbeat task-pickup: failed to list active tasks: {e}");
            return (true, has_high_priority);
        }
    };
    let my_active: Vec<_> = active_tasks
        .into_iter()
        .filter(|t| t.assigned_to.as_deref() == Some("heartbeat"))
        .filter(|t| t.execution != Execution::Deterministic)
        .collect();

    if eligible.is_empty() && my_active.is_empty() {
        tracing::debug!("Heartbeat task-pickup: no eligible tasks");
        return (false, false);
    }

    // ── Assisted / Full mode: claim and execute ─────────────────
    let actor = TaskActor {
        channel: "heartbeat".to_string(),
        id: Some("heartbeat".to_string()),
    };
    let audit = match crate::security::audit::AuditLogger::new(
        config.security.audit.clone(),
        config.workspace_dir.clone(),
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Heartbeat task-pickup: failed to create audit logger: {e}");
            return (true, has_high_priority);
        }
    };

    let mut had_error = false;

    // Process my-active tasks first (continuation turns using existing session)
    for task in &my_active {
        tracing::info!(
            "Heartbeat continuing active task {} (P{} {})",
            &task.id[..8.min(task.id.len())],
            task.priority,
            task.title,
        );

        let session_key = format!("task_{}", task.id);
        had_error |= execute_task_turn(config, task, &session_key, delivery, &audit).await;
    }

    // Then pick up new tasks from eligible
    for task in &eligible {
        // Claim via CAS — another agent may race us
        let claimed = match tasks::store::claim_task(workspace, &task.id, &actor, &audit) {
            Ok(t) => t,
            Err(tasks::TaskError::ClaimConflict { .. }) => {
                tracing::debug!(
                    "Heartbeat: task {} claimed by another agent, skipping",
                    &task.id[..8.min(task.id.len())]
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("Heartbeat: failed to claim task {}: {e}", &task.id[..8.min(task.id.len())]);
                had_error = true;
                continue;
            }
        };

        tracing::info!(
            "Heartbeat claimed task {} (P{} {})",
            &claimed.id[..8.min(claimed.id.len())],
            claimed.priority,
            claimed.title,
        );

        let session_key = format!("task_{}", claimed.id);
        had_error |= execute_task_turn(config, &claimed, &session_key, delivery, &audit).await;
    }

    (had_error, has_high_priority)
}

/// Execute a single agent turn for a task under its task binding, persisting
/// conversation in sessions.db under the given key. Returns `true` if an error occurred.
///
/// Enforces `max_task_turns`: increments turn_count before running, and if
/// the count exceeds the budget, blocks the task with "turn budget exhausted"
/// instead of running the LLM.
///
/// After the turn, re-reads the task status. If blocked and `notify_on_block`
/// is configured, emits a single channel notification — the ONLY permitted
/// channel emission from the task path.
async fn execute_task_turn(
    config: &Config,
    task: &crate::tasks::Task,
    session_key: &str,
    delivery: &Option<(String, String)>,
    audit: &crate::security::audit::AuditLogger,
) -> bool {
    use crate::tasks::{self, TaskActor, TaskBinding, TaskStatus};

    let workspace = &config.workspace_dir;
    let max_turns = config.heartbeat.max_task_turns;

    // Increment turn count before running
    let turn_count = match tasks::store::increment_turn_count(workspace, &task.id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Heartbeat: failed to increment turn_count for {}: {e}",
                &task.id[..8.min(task.id.len())]
            );
            return true;
        }
    };

    // Enforce turn budget (0 = unlimited = skip check)
    if max_turns > 0 && turn_count > max_turns {
        let actor = TaskActor {
            channel: "heartbeat".to_string(),
            id: Some("heartbeat".to_string()),
        };
        let reason = "turn budget exhausted";
        tracing::info!(
            "Heartbeat: task {} blocked — {} (turn {}/{})",
            &task.id[..8.min(task.id.len())],
            reason,
            turn_count,
            max_turns,
        );
        crate::health::mark_component_error(
            "heartbeat",
            format!("task {} turn budget exhausted", &task.id[..8.min(task.id.len())]),
        );
        let _ = tasks::store::block_task(workspace, &task.id, &actor, reason, audit);
        emit_notify_on_block(config, &task.id, delivery).await;
        return false; // Not an error per se — budget enforcement is expected
    }

    let intent = task.intent.as_deref().unwrap_or(&task.title);
    let prompt = format!(
        "[Heartbeat Task | P{}] {}\n\nIntent: {}",
        task.priority, task.title, intent,
    );

    let temp = daemonclaw_config::provider_store::get_fallback_provider()
        .as_ref()
        .and_then(|e| e.temperature)
        .unwrap_or(0.7);

    let binding = Some(TaskBinding {
        task_id: task.id.clone(),
        actor_id: "heartbeat".to_string(),
    });

    let session = crate::agent::SessionPersistence::Db(session_key.to_string());
    let agent_fut = tasks::with_task_binding(binding, async {
        crate::agent::run(
            config.clone(),
            Some(prompt),
            None,
            None,
            temp,
            vec![],
            false,
            session,
            None,
            daemonclaw_api::agent::TurnSource::Heartbeat,
        )
        .await
    });

    let result = if config.heartbeat.task_timeout_secs > 0 {
        match tokio::time::timeout(
            Duration::from_secs(config.heartbeat.task_timeout_secs),
            agent_fut,
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(
                "task {} timed out ({}s)",
                &task.id[..8.min(task.id.len())],
                config.heartbeat.task_timeout_secs,
            )),
        }
    } else {
        agent_fut.await
    };

    let had_error = match result {
        Ok(_output) => {
            crate::health::mark_component_ok("heartbeat");
            // No auto-submit — only the agent's task_submit tool moves to review.
            // No channel delivery — zero channel output on task path.
            // Task stays active until agent calls task_submit or task_block.
            false
        }
        Err(e) => {
            tracing::warn!(
                "Heartbeat: task {} failed: {e}",
                &task.id[..8.min(task.id.len())]
            );
            crate::health::mark_component_error(
                "heartbeat",
                format!("task {} failed: {e}", &task.id[..8.min(task.id.len())]),
            );
            true
        }
    };

    // After the turn, re-read the task. If it's now Blocked, emit notify_on_block.
    if let Ok(updated) = tasks::store::get_task(workspace, &task.id) {
        if updated.status == TaskStatus::Blocked {
            emit_notify_on_block(config, &task.id, delivery).await;
        }
    }

    had_error
}

/// Emit a single channel notification when a task becomes blocked.
/// This is the ONLY permitted channel emission from the task execution path.
async fn emit_notify_on_block(
    config: &Config,
    task_id: &str,
    delivery: &Option<(String, String)>,
) {
    if !config.heartbeat.notify_on_block {
        return;
    }
    if let Some((channel, target)) = delivery {
        let short_id = &task_id[..8.min(task_id.len())];
        let msg = format!("\u{26a0} task {short_id} blocked");
        let _ = crate::cron::scheduler::deliver_announcement(config, channel, target, &msg).await;
    }
}

/// Resolve delivery target: explicit config > auto-detect first configured channel.
fn resolve_heartbeat_delivery(config: &Config) -> Result<Option<(String, String)>> {
    let channel = config
        .heartbeat
        .target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target = config
        .heartbeat
        .to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (channel, target) {
        // Both explicitly set — validate and use.
        (Some(channel), Some(target)) => {
            validate_heartbeat_channel_config(config, channel)?;
            Ok(Some((channel.to_string(), target.to_string())))
        }
        // Only one set — error.
        (Some(_), None) => anyhow::bail!("heartbeat.to is required when heartbeat.target is set"),
        (None, Some(_)) => anyhow::bail!("heartbeat.target is required when heartbeat.to is set"),
        // Neither set — try auto-detect the first configured channel.
        (None, None) => Ok(auto_detect_heartbeat_channel(config)),
    }
}

/// Auto-detect the best channel for heartbeat delivery by checking which
/// channels are configured. Returns the first match in priority order.
fn auto_detect_heartbeat_channel(config: &Config) -> Option<(String, String)> {
    // Priority order: telegram > discord > slack > mattermost
    if let Some(tg) = &config.channels.telegram {
        // Use the first allowed_user as target, or fall back to empty (broadcast)
        let target = tg.allowed_users.first().cloned().unwrap_or_default();
        if !target.is_empty() {
            return Some(("telegram".to_string(), target));
        }
    }
    if config.channels.discord.is_some() {
        // Discord requires explicit target — can't auto-detect
        return None;
    }
    if config.channels.slack.is_some() {
        // Slack requires explicit target
        return None;
    }
    if config.channels.mattermost.is_some() {
        // Mattermost requires explicit target
        return None;
    }
    None
}

fn validate_heartbeat_channel_config(config: &Config, channel: &str) -> Result<()> {
    match channel.to_ascii_lowercase().as_str() {
        "telegram" => {
            if config.channels.telegram.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to telegram but channels.telegram is not configured"
                );
            }
        }
        "discord" => {
            if config.channels.discord.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to discord but channels.discord is not configured"
                );
            }
        }
        "slack" => {
            if config.channels.slack.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to slack but channels.slack is not configured"
                );
            }
        }
        "mattermost" => {
            if config.channels.mattermost.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to mattermost but channels.mattermost is not configured"
                );
            }
        }
        other => anyhow::bail!("unsupported heartbeat.target channel: {other}"),
    }

    Ok(())
}

fn has_supervised_channels(config: &Config) -> bool {
    config.channels.channels().iter().any(|(_, ok)| *ok)
}

// run_mqtt_sop_listener has been moved to daemonclaw-channels::orchestrator::mqtt.
// The daemon now receives it as a callback via DaemonSubsystems::mqtt_start.

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("boom")
        );
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
        assert!(
            component["last_error"]
                .as_str()
                .unwrap_or("")
                .contains("component exited unexpectedly")
        );
    }

    #[test]
    fn detects_no_supervised_channels() {
        let config = Config::default();
        assert!(!has_supervised_channels(&config));
    }

    #[test]
    fn detects_supervised_channels_present() {
        let mut config = Config::default();
        config.channels.telegram = Some(daemonclaw_config::schema::TelegramConfig {
            enabled: true,
            bot_token: "token".into(),
            allowed_users: vec![],
            stream_mode: daemonclaw_config::schema::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_dingtalk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.dingtalk = Some(daemonclaw_config::schema::DingTalkConfig {
            enabled: true,
            client_id: "client_id".into(),
            client_secret: "client_secret".into(),
            allowed_users: vec!["*".into()],
            proxy_url: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_mattermost_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.mattermost = Some(daemonclaw_config::schema::MattermostConfig {
            enabled: true,
            url: "https://mattermost.example.com".into(),
            bot_token: "token".into(),
            channel_id: Some("channel-id".into()),
            allowed_users: vec!["*".into()],
            thread_replies: Some(true),
            mention_only: Some(false),
            interrupt_on_new_message: false,
            proxy_url: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_qq_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.qq = Some(daemonclaw_config::schema::QQConfig {
            enabled: true,
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            allowed_users: vec!["*".into()],
            proxy_url: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_nextcloud_talk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.nextcloud_talk = Some(daemonclaw_config::schema::NextcloudTalkConfig {
            enabled: true,
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: None,
            allowed_users: vec!["*".into()],
            proxy_url: None,
            bot_name: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn webhook_only_config_is_supervised() {
        let mut config = Config::default();
        config.channels.webhook = Some(daemonclaw_config::schema::WebhookConfig {
            enabled: true,
            port: 8080,
            listen_path: None,
            send_url: None,
            send_method: None,
            auth_header: None,
            secret: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn resolve_delivery_none_when_unset() {
        let config = Config::default();
        let target = resolve_heartbeat_delivery(&config).unwrap();
        assert!(target.is_none());
    }

    #[test]
    fn resolve_delivery_requires_to_field() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("heartbeat.to is required when heartbeat.target is set")
        );
    }

    #[test]
    fn resolve_delivery_requires_target_field() {
        let mut config = Config::default();
        config.heartbeat.to = Some("123456".into());
        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("heartbeat.target is required when heartbeat.to is set")
        );
    }

    #[test]
    fn resolve_delivery_rejects_unsupported_channel() {
        let mut config = Config::default();
        config.heartbeat.target = Some("email".into());
        config.heartbeat.to = Some("ops@example.com".into());
        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported heartbeat.target channel")
        );
    }

    #[test]
    fn resolve_delivery_requires_channel_configuration() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        config.heartbeat.to = Some("123456".into());
        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("channels.telegram is not configured")
        );
    }

    #[test]
    fn resolve_delivery_accepts_telegram_configuration() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        config.heartbeat.to = Some("123456".into());
        config.channels.telegram = Some(daemonclaw_config::schema::TelegramConfig {
            enabled: true,
            bot_token: "bot-token".into(),
            allowed_users: vec![],
            stream_mode: daemonclaw_config::schema::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });

        let target = resolve_heartbeat_delivery(&config).unwrap();
        assert_eq!(target, Some(("telegram".to_string(), "123456".to_string())));
    }

    #[test]
    fn auto_detect_telegram_when_configured() {
        let mut config = Config::default();
        config.channels.telegram = Some(daemonclaw_config::schema::TelegramConfig {
            enabled: true,
            bot_token: "bot-token".into(),
            allowed_users: vec!["user123".into()],
            stream_mode: daemonclaw_config::schema::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });

        let target = resolve_heartbeat_delivery(&config).unwrap();
        assert_eq!(
            target,
            Some(("telegram".to_string(), "user123".to_string()))
        );
    }

    #[test]
    fn auto_detect_none_when_no_channels() {
        let config = Config::default();
        let target = auto_detect_heartbeat_channel(&config);
        assert!(target.is_none());
    }

    /// Verify that SIGHUP does not cause shutdown — the daemon should ignore it
    /// and only terminate on SIGINT or SIGTERM.
    #[cfg(unix)]
    #[tokio::test]
    async fn sighup_does_not_shut_down_daemon() {
        use libc;
        use tokio::time::{Duration, timeout};

        let handle = tokio::spawn(wait_for_shutdown_signal());

        // Give the signal handler time to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send SIGHUP to ourselves — should be ignored by the handler
        unsafe { libc::raise(libc::SIGHUP) };

        // The future should NOT complete within a short window
        let result = timeout(Duration::from_millis(200), handle).await;
        assert!(
            result.is_err(),
            "wait_for_shutdown_signal should not return after SIGHUP"
        );
    }
}
