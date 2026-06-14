use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use zeroclaw_config::schema::Config;
use zeroclaw_memory::{MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_OPEN};

mod registry;
pub use registry::DaemonRegistry;

const STATUS_FLUSH_SECONDS: u64 = 5;

/// Why the daemon's main loop returned.
///
/// `Shutdown`: process exits cleanly. `Reload`: caller (typically `src/main.rs`)
/// re-reads the config from disk and calls `daemon::run` again. The PID stays
/// the same; only the in-process subsystems get torn down and re-instantiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonExit {
    Shutdown,
    Reload,
}

/// Wait for either a shutdown signal (SIGINT / SIGTERM / Ctrl+C) or an
/// in-process reload signal (the gateway's `/admin/reload` writes `true`
/// on the watch channel). Returns the reason so the outer loop can decide
/// whether to re-init or exit. SIGHUP is ignored on Unix so the daemon
/// survives terminal / SSH disconnects.
///
/// The reload trigger is a tokio watch channel (not an OS signal) so it
/// works identically on Linux, macOS, and Windows. The Sender is owned by
/// the daemon (created in `run`) and cloned to the gateway for AppState.
/// Default grace period (seconds) before ephemeral shutdown after last client disconnects.
const EPHEMERAL_GRACE_SECS: u64 = 1;

async fn wait_for_exit_signal(
    mut reload_rx: tokio::sync::watch::Receiver<bool>,
    ephemeral: bool,
    client_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Result<DaemonExit> {
    use std::sync::atomic::Ordering;

    // Future that resolves when ephemeral shutdown is triggered:
    // waits for at least one client to connect, then for all clients to
    // disconnect, then sleeps the grace period. Pending forever if not
    // ephemeral.
    let ephemeral_shutdown = async {
        if !ephemeral {
            return std::future::pending::<()>().await;
        }
        // Wait until at least one client has connected.
        loop {
            if client_count.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // Wait until all clients disconnect.
        loop {
            if client_count.load(Ordering::Relaxed) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"grace_secs": EPHEMERAL_GRACE_SECS})),
            "All socket clients disconnected; starting ephemeral grace period"
        );
        // Grace period — if a client reconnects, abort.
        for _ in 0..EPHEMERAL_GRACE_SECS {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if client_count.load(Ordering::Relaxed) > 0 {
                // Client reconnected — restart the whole wait.
                return Box::pin(wait_for_ephemeral(client_count.clone())).await;
            }
        }
    };
    tokio::pin!(ephemeral_shutdown);

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sighup = signal(SignalKind::hangup())?;

        loop {
            tokio::select! {
                _ = sigint.recv() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Received SIGINT, shutting down...");
                    return Ok(DaemonExit::Shutdown);
                }
                _ = sigterm.recv() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Received SIGTERM, shutting down...");
                    return Ok(DaemonExit::Shutdown);
                }
                _ = sighup.recv() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Received SIGHUP, ignoring (daemon stays running)");
                }
                changed = reload_rx.changed() => {
                    if changed.is_err() {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "Reload sender dropped; shutting down");
                        return Ok(DaemonExit::Shutdown);
                    }
                    if *reload_rx.borrow_and_update() {
                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Reload requested via /admin/reload");
                        return Ok(DaemonExit::Reload);
                    }
                }
                _ = &mut ephemeral_shutdown => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Ephemeral daemon: no clients remaining, shutting down");
                    return Ok(DaemonExit::Shutdown);
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        loop {
            tokio::select! {
                res = tokio::signal::ctrl_c() => {
                    res?;
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Received Ctrl+C, shutting down...");
                    return Ok(DaemonExit::Shutdown);
                }
                changed = reload_rx.changed() => {
                    if changed.is_err() {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "Reload sender dropped; shutting down");
                        return Ok(DaemonExit::Shutdown);
                    }
                    if *reload_rx.borrow_and_update() {
                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Reload requested via /admin/reload");
                        return Ok(DaemonExit::Reload);
                    }
                }
                _ = &mut ephemeral_shutdown => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Ephemeral daemon: no clients remaining, shutting down");
                    return Ok(DaemonExit::Shutdown);
                }
            }
        }
    }
}

/// Recursive helper: wait for clients to connect then all disconnect, with grace period.
async fn wait_for_ephemeral(client_count: std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    // Wait until all clients disconnect again.
    loop {
        if client_count.load(Ordering::Relaxed) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"grace_secs": EPHEMERAL_GRACE_SECS})),
        "All socket clients disconnected; starting ephemeral grace period"
    );
    for _ in 0..EPHEMERAL_GRACE_SECS {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if client_count.load(Ordering::Relaxed) > 0 {
            return Box::pin(wait_for_ephemeral(client_count)).await;
        }
    }
}

pub async fn run(
    config: Config,
    host: String,
    port: u16,
    mut registry: DaemonRegistry,
    ephemeral: bool,
) -> Result<DaemonExit> {
    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    // Shared broadcast channel so all daemon components (gateway, cron,
    // heartbeat) can publish real-time events to dashboard clients.
    let (event_tx, _rx) = tokio::sync::broadcast::channel::<serde_json::Value>(256);

    // Wire the log broadcast hook so every record!() emission reaches the
    // RPC logs/subscribe stream. Without this, tool calls and agent events
    // logged via record!() are invisible to the zerocode Logs pane when
    // connected over the Unix socket (the gateway wires this separately for
    // its own event_tx; the daemon's RPC event_tx must be wired here).
    zeroclaw_log::set_broadcast_hook(event_tx.clone());

    if config.heartbeat.enabled {
        let _ = crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&config.data_dir)
            .await;
    }

    let mut handles: Vec<JoinHandle<()>> = vec![spawn_state_writer(config.clone())];

    // Reload channel: gateway's /admin/reload writes here; our wait loop
    // (below) selects on it alongside OS signals. Cross-platform.
    let (reload_tx, reload_rx) = tokio::sync::watch::channel::<bool>(false);

    // Construct the TUI registry early so both the gateway (for /api/tuis)
    // and the RPC socket (for tui/list) share the same Arc.
    let tui_registry =
        std::sync::Arc::new(crate::rpc::tui_identity::TuiRegistry::new(&config.data_dir));

    if let Some(gateway_start) = registry.take_gateway_start() {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        let gateway_event_tx = event_tx.clone();
        let gateway_reload_tx = reload_tx.clone();
        let gateway_tui_registry = tui_registry.clone();
        let gateway_start = std::sync::Arc::new(gateway_start);
        handles.push(spawn_component_supervisor(
            "gateway",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = gateway_cfg.clone();
                let host = gateway_host.clone();
                let tx = gateway_event_tx.clone();
                let reload = gateway_reload_tx.clone();
                let tui_reg = gateway_tui_registry.clone();
                let start = gateway_start.clone();
                async move { start(host, port, cfg, Some(tx), Some(reload), Some(tui_reg)).await }
            },
        ));
    }

    let channels_cancel = tokio_util::sync::CancellationToken::new();

    if let Some(channels_start) = registry.take_channels_start() {
        if has_supervised_channels(&config) {
            let channels_cfg = config.clone();
            let channels_start = std::sync::Arc::new(channels_start);
            let cancel_for_supervisor = channels_cancel.clone();
            handles.push(spawn_component_supervisor(
                "channels",
                initial_backoff,
                max_backoff,
                move || {
                    let cfg = channels_cfg.clone();
                    let start = channels_start.clone();
                    let cancel = cancel_for_supervisor.clone();
                    async move { start(cfg, cancel).await }
                },
            ));
        } else {
            crate::health::mark_component_ok("channels");
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "No channels configured; channel supervisor disabled"
            );
        }
    } else {
        crate::health::mark_component_ok("channels");
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Channels subsystem not wired; channel supervisor disabled"
        );
    }

    // RPC transports: Unix socket (#6837) and WSS (remote TUI connections).
    // Build the shared RpcContext if either transport is configured.
    let socket_client_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let need_rpc_ctx = registry.has_socket_start() || registry.has_wss_start();

    // Extract shared SOP engine from registry for RpcContext.
    let (sop_engine, sop_audit) = registry.take_sop_engine();

    let rpc_ctx = if need_rpc_ctx {
        use crate::rpc::context::RpcContext;
        use crate::rpc::session::SessionStore;
        use zeroclaw_infra::session_queue::SessionActorQueue;

        let session_queue = std::sync::Arc::new(SessionActorQueue::new(32, 30, 600));
        let sessions = std::sync::Arc::new(SessionStore::new(64, session_queue.clone()));

        {
            let reaper_queue = std::sync::Arc::clone(&session_queue);
            zeroclaw_spawn::spawn!(async move {
                const TICK: std::time::Duration = std::time::Duration::from_secs(60);
                let mut interval = tokio::time::interval(TICK);
                interval.tick().await;
                loop {
                    interval.tick().await;
                    let queue_evicted = reaper_queue.evict_idle().await;
                    if queue_evicted > 0 {
                        let span = ::zeroclaw_log::info_span!(
                            target: "zeroclaw_log_internal_scope",
                            "zeroclaw_scope",
                            channel = "rpc",
                        );
                        let _guard = span.enter();
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note,
                            )
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_attrs(::serde_json::json!({
                                "evicted_queue_slots": queue_evicted,
                            })),
                            "Session queue: released idle actor-queue slots"
                        );
                        crate::util::release_freed_heap();
                    }
                }
            });
        }
        let session_backend = zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        )
        .ok();

        // Wire the memory subsystem so `memory/list` and `memory/search`
        // work over RPC transports (same pattern as the gateway).
        let rpc_memory: Option<std::sync::Arc<dyn zeroclaw_api::memory_traits::Memory>> = if config
            .agents
            .is_empty()
        {
            None
        } else {
            match zeroclaw_memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.embedding_routes,
                config.resolve_active_storage(),
                &config.data_dir,
                None,
            ) {
                Ok(mem) => Some(std::sync::Arc::from(mem)),
                Err(_e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "RPC memory subsystem unavailable"
                    );
                    None
                }
            }
        };

        // Open the ACP session DB at boot so the file exists from the
        // moment the daemon is up, not when (if ever) `zeroclaw acp`
        // runs. Best-effort: on failure, log and continue with `None`.
        let acp_session_store: Option<
            std::sync::Arc<zeroclaw_infra::acp_session_store::AcpSessionStore>,
        > = match zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir) {
            Ok(s) => Some(std::sync::Arc::new(s)),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "Failed to open ACP session store at daemon boot"
                );
                None
            }
        };

        Some(std::sync::Arc::new(RpcContext {
            config: std::sync::Arc::new(parking_lot::RwLock::new(config.clone())),
            sessions,
            session_backend,
            memory: rpc_memory,
            cost_tracker: None, // TODO: wire when cost tracker is daemon-scoped
            event_tx: Some(event_tx.clone()),
            reload_tx: Some(reload_tx.clone()),
            approval_pending: std::sync::Arc::new(
                crate::rpc::context::ApprovalPendingMap::default(),
            ),
            tui_registry,
            acp_session_store,
            sop_engine,
            sop_audit,
        }))
    } else {
        None
    };

    // Local IPC RPC listener (Unix socket on Unix, Named Pipe on Windows).
    if let Some(socket_start) = registry.take_socket_start() {
        let rpc_ctx = rpc_ctx
            .clone()
            .expect("rpc_ctx built when socket_start is Some");
        let socket_start = std::sync::Arc::new(socket_start);
        let socket_cancel = channels_cancel.clone();
        let count = socket_client_count.clone();
        handles.push(spawn_component_supervisor(
            "socket",
            initial_backoff,
            max_backoff,
            move || {
                let ctx = rpc_ctx.clone();
                let start = socket_start.clone();
                let cancel = socket_cancel.clone();
                let count = count.clone();
                async move { start(ctx, cancel, count).await }
            },
        ));
    }

    // WSS RPC listener (remote TUI connections).
    if let Some(wss_start) = registry.take_wss_start() {
        let rpc_ctx = rpc_ctx
            .clone()
            .expect("rpc_ctx built when wss_start is Some");
        let wss_start = std::sync::Arc::new(wss_start);
        let wss_cancel = channels_cancel.clone();
        let count = socket_client_count.clone();
        handles.push(spawn_component_supervisor(
            "wss",
            initial_backoff,
            max_backoff,
            move || {
                let ctx = rpc_ctx.clone();
                let start = wss_start.clone();
                let cancel = wss_cancel.clone();
                let count = count.clone();
                async move { start(ctx, cancel, count).await }
            },
        ));
    }

    // Wire up MQTT SOP listener if configured and referenced by an enabled agent
    if let Some(mqtt_start) = registry.take_mqtt_start() {
        let active_mqtt: std::collections::HashSet<String> = config
            .agents
            .values()
            .filter(|a| a.enabled)
            .flat_map(|a| a.channels.iter().map(|c| c.as_str().to_string()))
            .collect();
        let mut mqtt_started = false;
        for (alias, mqtt_config) in &config.channels.mqtt {
            if !active_mqtt.contains(&format!("mqtt.{alias}")) {
                continue;
            }
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
            mqtt_started = true;
            break;
        }
        if !mqtt_started {
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

    if config.scheduler.enabled {
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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Cron disabled; scheduler supervisor not started"
        );
    }

    println!("🧠 ZeroClaw daemon started");
    println!("   Gateway:  http://{host}:{port}");
    println!(
        "   Socket:   {}",
        crate::rpc::local::socket_path(&config).display()
    );
    println!("   Components: gateway, channels, heartbeat, scheduler");
    if config.gateway.require_pairing {
        println!("   Pairing:    enabled (code appears in gateway output above)");
    }
    println!("   Ctrl+C or SIGTERM to stop");

    // Wait for shutdown (SIGINT/SIGTERM/Ctrl+C) or reload (in-process channel).
    let exit = wait_for_exit_signal(reload_rx, ephemeral, socket_client_count).await?;
    crate::health::mark_component_error(
        "daemon",
        match exit {
            DaemonExit::Shutdown => "shutdown requested",
            DaemonExit::Reload => "reload requested",
        },
    );

    // Fire channel cancellation before aborting supervisors so listener tasks
    // get a chance to drop their `Arc<dyn Channel>` (and the matrix-sdk SQLite
    // pools the Arc transitively pins).
    channels_cancel.cancel();
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }

    Ok(exit)
}

pub fn state_file_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("state")
        .join("daemon_state.json")
}

fn spawn_state_writer(config: Config) -> JoinHandle<()> {
    zeroclaw_spawn::spawn!(async move {
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
    zeroclaw_spawn::spawn!(async move {
        let mut backoff = initial_backoff_secs.max(1);
        let max_backoff = max_backoff_secs.max(backoff);

        loop {
            crate::health::mark_component_ok(name);
            match run_component().await {
                Ok(()) => {
                    crate::health::mark_component_error(name, "component exited unexpectedly");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"name": name})),
                        &format!("Daemon component '{name}' exited unexpectedly")
                    );
                    // Clean exit — reset backoff since the component ran successfully
                    backoff = initial_backoff_secs.max(1);
                }
                Err(e) => {
                    crate::health::mark_component_error(name, e.to_string());
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "name": name})
                            ),
                        &format!("Daemon component '{name}' failed: {e}")
                    );
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
    use crate::heartbeat::engine::{
        HeartbeatEngine, HeartbeatTask, TaskPriority, TaskStatus, compute_adaptive_interval,
    };
    use std::sync::Arc;

    let agent_alias = config.heartbeat.agent.trim().to_string();
    if agent_alias.is_empty() {
        anyhow::bail!(
            "heartbeat worker requires `[heartbeat] agent = \"<alias>\"` naming a configured agent"
        );
    }
    if config.agent(&agent_alias).is_none() {
        anyhow::bail!(
            "[heartbeat] agent = {agent_alias:?} is not configured ([agents.{agent_alias}] missing)"
        );
    }

    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = HeartbeatEngine::new(config.heartbeat.clone(), config.data_dir.clone(), observer);
    let metrics = engine.metrics();
    let delivery = resolve_heartbeat_delivery(&config)?;
    let two_phase = config.heartbeat.two_phase;
    let adaptive = config.heartbeat.adaptive;
    let start_time = std::time::Instant::now();

    // ── Deadman watcher ──────────────────────────────────────────
    let deadman_timeout = config.heartbeat.deadman_timeout_minutes;
    if deadman_timeout > 0 {
        let dm_metrics = Arc::clone(&metrics);
        let dm_config = config.clone();
        let dm_delivery = delivery.clone();
        zeroclaw_spawn::spawn!(async move {
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
                        &dm_config, &channel, &target, None, &alert,
                    );
                    match tokio::time::timeout(Duration::from_secs(30), delivery_fut).await {
                        Ok(Err(e)) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "Deadman alert delivery failed"
                            );
                        }
                        Err(_) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                "Deadman alert delivery timed out (30s)"
                            );
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

        // Collect runnable tasks (active only, sorted by priority)
        let mut tasks = engine.collect_runnable_tasks().await?;
        let has_high_priority = tasks.iter().any(|t| t.priority == TaskPriority::High);

        if tasks.is_empty() {
            if let Some(fallback) = config
                .heartbeat
                .message
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
            {
                tasks.push(HeartbeatTask {
                    text: fallback.to_string(),
                    priority: TaskPriority::Medium,
                    status: TaskStatus::Active,
                });
            } else {
                #[allow(clippy::cast_precision_loss)]
                let elapsed = tick_start.elapsed().as_millis() as f64;
                metrics.lock().record_success(elapsed);
                continue;
            }
        }

        // ── Phase 1: LLM decision (two-phase mode) ──────────────
        let tasks_to_run = if two_phase {
            let decision_prompt = format!(
                "[Heartbeat Task | decision] {}",
                HeartbeatEngine::build_decision_prompt(&tasks),
            );
            let phase1_fut = Box::pin(crate::agent::run(
                config.clone(),
                &agent_alias,
                Some(decision_prompt),
                None,
                None,
                Some(0.0),
                vec![],
                false,
                None,
                None,
                crate::agent::loop_::AgentRunOverrides::default(),
            ));
            let phase1_result = if config.heartbeat.task_timeout_secs > 0 {
                match tokio::time::timeout(
                    Duration::from_secs(config.heartbeat.task_timeout_secs),
                    phase1_fut,
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Timeout
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "phase": "phase1_decision",
                                "timeout_secs": config.heartbeat.task_timeout_secs,
                            })),
                            "heartbeat: phase1 decision timed out"
                        );
                        Err(anyhow::Error::msg(format!(
                            "Phase 1 decision timed out ({}s)",
                            config.heartbeat.task_timeout_secs
                        )))
                    }
                }
            } else {
                phase1_fut.await
            };
            match phase1_result {
                Ok(response) => {
                    let indices = HeartbeatEngine::parse_decision_response(&response, tasks.len());
                    if indices.is_empty() {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            "heartbeat phase 1: skip (nothing to do)"
                        );
                        crate::health::mark_component_ok("heartbeat");
                        #[allow(clippy::cast_precision_loss)]
                        let elapsed = tick_start.elapsed().as_millis() as f64;
                        metrics.lock().record_success(elapsed);
                        continue;
                    }
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"selected": indices.len(), "total": tasks.len()})), "heartbeat phase 1: running task subset");
                    indices
                        .into_iter()
                        .filter_map(|i| tasks.get(i).cloned())
                        .collect()
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "heartbeat phase 1 failed; running all tasks"
                    );
                    tasks
                }
            }
        } else {
            tasks
        };

        // ── Phase 2: Execute selected tasks ─────────────────────
        // Re-read session context on every tick so we pick up messages
        // that arrived since the daemon started.
        let session_context = if config.heartbeat.load_session_context {
            load_heartbeat_session_context(&config)
        } else {
            None
        };

        // Create memory once per tick for recall + consolidation.
        let heartbeat_memory: Option<Box<dyn zeroclaw_memory::Memory>> =
            zeroclaw_memory::create_memory(
                &config.memory,
                &config.data_dir,
                config
                    .model_provider_for_agent(&agent_alias)
                    .and_then(|e| e.api_key.as_deref()),
            )
            .ok();

        let mut tick_had_error = false;
        for task in &tasks_to_run {
            let task_start = std::time::Instant::now();
            let task_prompt = format!("[Heartbeat Task | {}] {}", task.priority, task.text);

            // Recall relevant memories so heartbeat tasks have context awareness.
            // Exclude `Conversation` memories to prevent chat context from
            // leaking into scheduled executions.
            let memory_context = if let Some(ref mem) = heartbeat_memory {
                match mem.recall(&task.text, 5, None, None, None).await {
                    Ok(entries) if !entries.is_empty() => {
                        let ctx: String = entries
                            .iter()
                            .filter(|e| {
                                !matches!(
                                    e.category,
                                    zeroclaw_memory::traits::MemoryCategory::Conversation
                                )
                            })
                            .map(|e| format!("- {}: {}", e.key, e.content))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if ctx.is_empty() {
                            None
                        } else {
                            Some(format!(
                                "{MEMORY_CONTEXT_OPEN}\n{ctx}\n{MEMORY_CONTEXT_CLOSE}\n\n"
                            ))
                        }
                    }
                    _ => None,
                }
            } else {
                None
            };

            let prompt = match (&session_context, &memory_context) {
                (Some(sc), Some(mc)) => format!("{mc}\n{sc}\n\n{task_prompt}"),
                (Some(sc), None) => format!("{sc}\n\n{task_prompt}"),
                (None, Some(mc)) => format!("{mc}\n\n{task_prompt}"),
                (None, None) => task_prompt,
            };
            let temp: Option<f64> = config
                .model_provider_for_agent(&agent_alias)
                .and_then(|e| e.temperature);
            let phase2_fut = Box::pin(crate::agent::run(
                config.clone(),
                &agent_alias,
                Some(prompt),
                None,
                None,
                temp,
                vec![],
                false,
                None,
                None,
                crate::agent::loop_::AgentRunOverrides::default(),
            ));
            let phase2_result = if config.heartbeat.task_timeout_secs > 0 {
                match tokio::time::timeout(
                    Duration::from_secs(config.heartbeat.task_timeout_secs),
                    phase2_fut,
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Timeout
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "phase": "phase2_heartbeat",
                                "timeout_secs": config.heartbeat.task_timeout_secs,
                            })),
                            "heartbeat task timed out"
                        );
                        Err(anyhow::Error::msg(format!(
                            "Heartbeat task timed out ({}s)",
                            config.heartbeat.task_timeout_secs
                        )))
                    }
                }
            } else {
                phase2_fut.await
            };
            match phase2_result {
                Ok(output) => {
                    crate::health::mark_component_ok("heartbeat");
                    #[allow(clippy::cast_possible_truncation)]
                    let duration_ms = task_start.elapsed().as_millis() as i64;
                    let now = chrono::Utc::now();
                    let _ = crate::heartbeat::store::record_run(
                        &config.data_dir,
                        &task.text,
                        &task.priority.to_string(),
                        now - chrono::Duration::milliseconds(duration_ms),
                        now,
                        "ok",
                        Some(output.as_str()),
                        duration_ms,
                        config.heartbeat.max_run_history,
                    );
                    // Consolidate heartbeat output to memory for cross-session awareness.
                    if config.memory.auto_save
                        && output.chars().count() >= 50
                        && let Some(ref mem) = heartbeat_memory
                    {
                        let key = format!("heartbeat_{}", uuid::Uuid::new_v4());
                        let summary = if output.len() > 500 {
                            // Find a valid UTF-8 char boundary at or before 500.
                            let mut end = 500;
                            while end > 0 && !output.is_char_boundary(end) {
                                end -= 1;
                            }
                            &output[..end]
                        } else {
                            &output
                        };
                        let _ = mem
                            .store(
                                &key,
                                &format!("Heartbeat task '{}': {}", task.text, summary),
                                zeroclaw_memory::MemoryCategory::Daily,
                                None,
                            )
                            .await;
                    }

                    let announcement = if output.trim().is_empty() {
                        format!("💓 heartbeat task completed: {}", task.text)
                    } else {
                        output
                    };
                    if let Some((channel, target)) = &delivery {
                        let delivery_result = tokio::time::timeout(
                            Duration::from_secs(30),
                            crate::cron::scheduler::deliver_announcement(
                                &config,
                                channel,
                                target,
                                None,
                                &announcement,
                            ),
                        )
                        .await;
                        match delivery_result {
                            Ok(Err(e)) => {
                                crate::health::mark_component_error(
                                    "heartbeat",
                                    format!("delivery failed: {e}"),
                                );
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "Heartbeat delivery failed"
                                );
                            }
                            Err(_) => {
                                crate::health::mark_component_error(
                                    "heartbeat",
                                    "delivery timed out (30s)".to_string(),
                                );
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                    "Heartbeat delivery timed out (30s)"
                                );
                            }
                            Ok(Ok(())) => {}
                        }
                    }
                }
                Err(e) => {
                    tick_had_error = true;
                    #[allow(clippy::cast_possible_truncation)]
                    let duration_ms = task_start.elapsed().as_millis() as i64;
                    let now = chrono::Utc::now();
                    let _ = crate::heartbeat::store::record_run(
                        &config.data_dir,
                        &task.text,
                        &task.priority.to_string(),
                        now - chrono::Duration::milliseconds(duration_ms),
                        now,
                        "error",
                        Some(&e.to_string()),
                        duration_ms,
                        config.heartbeat.max_run_history,
                    );
                    crate::health::mark_component_error("heartbeat", e.to_string());
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Heartbeat task failed"
                    );
                }
            }
        }

        // Update metrics
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

        // Compute next sleep interval
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

/// Load recent conversation history for the heartbeat's delivery target and
/// format it as a text preamble to inject into the task prompt.
///
/// Scans `{workspace}/sessions/` for JSONL files whose name starts with
/// `{channel}_` and ends with `_{to}.jsonl` (or exactly `{channel}_{to}.jsonl`),
/// then picks the most recently modified match. This handles session key
/// formats such as `telegram_diskiller.jsonl` and
/// `telegram_5673725398_diskiller.jsonl`.
/// Returns `None` when `target`/`to` are not configured or no session exists.
const HEARTBEAT_SESSION_CONTEXT_MESSAGES: usize = 20;

fn load_heartbeat_session_context(config: &Config) -> Option<String> {
    use zeroclaw_providers::traits::ChatMessage;

    let channel = config
        .heartbeat
        .target
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())?;
    let to = config
        .heartbeat
        .to
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())?;

    if channel.contains('/') || channel.contains('\\') || to.contains('/') || to.contains('\\') {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "heartbeat session context: channel/to contains path separators, skipping"
        );
        return None;
    }

    let sessions_dir = config.data_dir.join("sessions");

    // Find the most recently modified JSONL file that belongs to this target.
    // Matches both `{channel}_{to}.jsonl` and `{channel}_{anything}_{to}.jsonl`.
    let prefix = format!("{channel}_");
    let suffix = format!("_{to}.jsonl");
    let exact = format!("{channel}_{to}.jsonl");
    let mid_prefix = format!("{channel}_{to}_");

    let path = std::fs::read_dir(&sessions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".jsonl")
                && (name == exact
                    || (name.starts_with(&prefix) && name.ends_with(&suffix))
                    || name.starts_with(&mid_prefix))
        })
        .max_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
        .map(|e| e.path())?;

    if !path.exists() {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"channel": channel, "to": to})),
            "heartbeat session context: no session file found"
        );
        return None;
    }

    let messages = load_jsonl_messages(&path);
    if messages.is_empty() {
        return None;
    }

    let recent: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .rev()
        .take(HEARTBEAT_SESSION_CONTEXT_MESSAGES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Only inject context if there is at least one real user message in the
    // window. If the JSONL contains only assistant messages (e.g. previous
    // heartbeat outputs with no reply yet), skip context to avoid feeding
    // Monika's own messages back to her in a loop.
    let has_user_message = recent.iter().any(|m| m.role == "user");
    if !has_user_message {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "💓 Heartbeat session context: no user messages in recent history — skipping"
        );
        return None;
    }

    // Use the session file's mtime as a proxy for when the last message arrived.
    let last_message_age = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|mtime| mtime.elapsed().ok());

    let silence_note = match last_message_age {
        Some(age) => {
            let mins = age.as_secs() / 60;
            if mins < 60 {
                format!("(last message ~{mins} minutes ago)\n")
            } else {
                let hours = mins / 60;
                let rem = mins % 60;
                if rem == 0 {
                    format!("(last message ~{hours}h ago)\n")
                } else {
                    format!("(last message ~{hours}h {rem}m ago)\n")
                }
            }
        }
        None => String::new(),
    };

    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        &format!(
            "💓 Heartbeat session context: {} messages from {}, silence: {}",
            recent.len(),
            path.display().to_string(),
            silence_note.trim()
        )
    );

    let mut ctx = format!(
        "[Recent conversation history — use this for context when composing your message] {silence_note}",
    );
    for msg in &recent {
        let label = if msg.role == "user" { "User" } else { "You" };
        // Truncate very long messages to avoid bloating the prompt.
        // Use char_indices to avoid panicking on multi-byte UTF-8 characters.
        let content = if msg.content.len() > 500 {
            let truncate_at = msg
                .content
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 500)
                .last()
                .unwrap_or(0);
            format!("{}…", &msg.content[..truncate_at])
        } else {
            msg.content.clone()
        };
        ctx.push_str(label);
        ctx.push_str(": ");
        ctx.push_str(&content);
        ctx.push('\n');
    }

    Some(ctx)
}

/// Read the last `HEARTBEAT_SESSION_CONTEXT_MESSAGES` `ChatMessage` lines from
/// a JSONL session file using a bounded rolling window so we never hold the
/// entire file in memory.
fn load_jsonl_messages(path: &std::path::Path) -> Vec<zeroclaw_providers::traits::ChatMessage> {
    use std::collections::VecDeque;
    use std::io::BufRead;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(file);
    let mut window: VecDeque<zeroclaw_providers::traits::ChatMessage> =
        VecDeque::with_capacity(HEARTBEAT_SESSION_CONTEXT_MESSAGES + 1);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<zeroclaw_providers::traits::ChatMessage>(trimmed) {
            window.push_back(msg);
            if window.len() > HEARTBEAT_SESSION_CONTEXT_MESSAGES {
                window.pop_front();
            }
        }
    }
    window.into_iter().collect()
}

/// Auto-detect the best channel for heartbeat delivery by checking which
/// channels are configured. Returns the first match in priority order.
fn auto_detect_heartbeat_channel(config: &Config) -> Option<(String, String)> {
    // Priority order: telegram > discord > slack > mattermost
    // Find the first external peer authorized on a telegram channel
    // (peer authorization lives in peer_groups in V3, not on the
    // channel block).
    if !config.channels.telegram.is_empty() {
        for alias in config.channels.telegram.keys() {
            let peers = config.channel_external_peers("telegram", alias);
            if let Some(target) = peers.into_iter().next() {
                return Some(("telegram".to_string(), target));
            }
        }
    }
    if !config.channels.discord.is_empty() {
        // Discord requires explicit target — can't auto-detect
        return None;
    }
    if !config.channels.slack.is_empty() {
        // Slack requires explicit target
        return None;
    }
    if !config.channels.mattermost.is_empty() {
        // Mattermost requires explicit target
        return None;
    }
    None
}

fn validate_heartbeat_channel_config(config: &Config, channel: &str) -> Result<()> {
    match channel.to_ascii_lowercase().as_str() {
        "telegram" => {
            if config.channels.telegram.is_empty() {
                anyhow::bail!(
                    "heartbeat.target is set to telegram but channels.telegram is not configured"
                );
            }
        }
        "discord" => {
            if config.channels.discord.is_empty() {
                anyhow::bail!(
                    "heartbeat.target is set to discord but channels.discord is not configured"
                );
            }
        }
        "slack" => {
            if config.channels.slack.is_empty() {
                anyhow::bail!(
                    "heartbeat.target is set to slack but channels.slack is not configured"
                );
            }
        }
        "mattermost" => {
            if config.channels.mattermost.is_empty() {
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
    // Check that at least one channel entry has `enabled = true`.
    // A config with only `enabled = false` entries (e.g. partially-configured
    // or intentionally disabled bots) must not start the supervisor — the
    // channels component would find nothing to listen on, return Ok(()), and
    // the daemon supervisor would restart it in a tight loop.
    config.channels.has_any_enabled()
}

// run_mqtt_sop_listener has been moved to zeroclaw-channels::orchestrator::mqtt.
// The daemon now receives it as a starter via DaemonRegistry::register_mqtt.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config
    }

    #[test]
    fn state_file_path_uses_config_state_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let path = state_file_path(&config);
        assert_eq!(path, tmp.path().join("state").join("daemon_state.json"));
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
    fn all_disabled_channels_not_supervised() {
        // Regression test: a config with channel entries that all have
        // `enabled = false` must not start the channels supervisor.
        // Previously, has_supervised_channels only checked map non-emptiness,
        // causing the supervisor to start, find nothing to listen on, return
        // Ok(()), and restart in a tight loop.
        let mut config = Config::default();
        config.channels.discord.insert(
            "clamps".to_string(),
            zeroclaw_config::schema::DiscordConfig {
                enabled: false,
                bot_token: "token".into(),
                guild_ids: vec![],
                channel_ids: vec![],
                listen_to_bots: false,
                mention_only: true,
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 0,
                multi_message_delay_ms: 0,
                stall_timeout_secs: 0,
                interrupt_on_new_message: false,
                archive: false,
                approval_timeout_secs: 0,
                proxy_url: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        config.channels.discord.insert(
            "glados".to_string(),
            zeroclaw_config::schema::DiscordConfig {
                enabled: false,
                bot_token: "token2".into(),
                guild_ids: vec![],
                channel_ids: vec![],
                listen_to_bots: false,
                mention_only: true,
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 0,
                multi_message_delay_ms: 0,
                stall_timeout_secs: 0,
                interrupt_on_new_message: false,
                archive: false,
                approval_timeout_secs: 0,
                proxy_url: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        assert!(!has_supervised_channels(&config));
    }

    #[test]
    fn detects_supervised_channels_present() {
        let mut config = Config::default();
        config.channels.telegram.insert(
            "default".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "token".into(),
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 1000,
                interrupt_on_new_message: false,
                mention_only: false,
                ack_reactions: None,
                proxy_url: None,
                approval_timeout_secs: 120,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_dingtalk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.dingtalk.insert(
            "default".to_string(),
            zeroclaw_config::schema::DingTalkConfig {
                enabled: true,
                client_id: "client_id".into(),
                client_secret: "client_secret".into(),
                proxy_url: None,
                excluded_tools: vec![],
            },
        );
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_mattermost_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.mattermost.insert(
            "default".to_string(),
            zeroclaw_config::schema::MattermostConfig {
                enabled: true,
                url: "https://mattermost.example.com".into(),
                bot_token: Some("token".into()),
                login_id: None,
                password: None,
                channel_ids: vec!["channel-id".into()],
                team_ids: vec![],
                discover_dms: None,
                thread_replies: Some(true),
                mention_only: Some(false),
                interrupt_on_new_message: false,
                proxy_url: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_qq_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.qq.insert(
            "default".to_string(),
            zeroclaw_config::schema::QQConfig {
                enabled: true,
                app_id: "app-id".into(),
                app_secret: "app-secret".into(),
                proxy_url: None,
                excluded_tools: vec![],
            },
        );
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_nextcloud_talk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels.nextcloud_talk.insert(
            "default".to_string(),
            zeroclaw_config::schema::NextcloudTalkConfig {
                enabled: true,
                base_url: "https://cloud.example.com".into(),
                app_token: "app-token".into(),
                webhook_secret: None,
                proxy_url: None,
                bot_name: None,
                excluded_tools: vec![],
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 1000,
            },
        );
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn webhook_only_config_is_supervised() {
        let mut config = Config::default();
        config.channels.webhook.insert(
            "default".to_string(),
            zeroclaw_config::schema::WebhookConfig {
                enabled: true,
                port: 8080,
                listen_path: None,
                send_url: None,
                send_method: None,
                auth_header: None,
                secret: None,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
                max_retries: None,
                retry_base_delay_ms: None,
                retry_max_delay_ms: None,
            },
        );
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
        config.channels.telegram.insert(
            "default".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "bot-token".into(),
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 1000,
                interrupt_on_new_message: false,
                mention_only: false,
                ack_reactions: None,
                proxy_url: None,
                approval_timeout_secs: 120,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );

        let target = resolve_heartbeat_delivery(&config).unwrap();
        assert_eq!(target, Some(("telegram".to_string(), "123456".to_string())));
    }

    #[test]
    fn auto_detect_telegram_when_configured() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};

        let mut config = Config::default();
        config.channels.telegram.insert(
            "default".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "bot-token".into(),
                stream_mode: zeroclaw_config::schema::StreamMode::default(),
                draft_update_interval_ms: 1000,
                interrupt_on_new_message: false,
                mention_only: false,
                ack_reactions: None,
                proxy_url: None,
                approval_timeout_secs: 120,
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            },
        );
        // Inbound peer authorization lives in peer_groups in V3.
        // Auto-detect picks the first external peer of the synthesized
        // `telegram_default` group as the heartbeat target.
        config.peer_groups.insert(
            "telegram_default".to_string(),
            PeerGroupConfig {
                channel: "telegram".to_string(),
                external_peers: vec![PeerUsername::new("user123")],
                ..PeerGroupConfig::default()
            },
        );

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

        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(false);
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = zeroclaw_spawn::spawn!(wait_for_exit_signal(reload_rx, false, count));

        // Give the signal handler time to register
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send SIGHUP to ourselves — should be ignored by the handler
        unsafe { libc::raise(libc::SIGHUP) };

        // The future should NOT complete within a short window
        let result = timeout(Duration::from_millis(200), handle).await;
        assert!(
            result.is_err(),
            "wait_for_exit_signal should not return after SIGHUP"
        );
    }

    /// In-process reload channel returns DaemonExit::Reload so the outer
    /// loop can re-init. Cross-platform — works on Linux, macOS, Windows.
    #[tokio::test]
    async fn reload_channel_returns_reload() {
        use tokio::time::{Duration, timeout};

        let (reload_tx, reload_rx) = tokio::sync::watch::channel(false);
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = zeroclaw_spawn::spawn!(wait_for_exit_signal(reload_rx, false, count));
        tokio::time::sleep(Duration::from_millis(50)).await;
        reload_tx.send(true).expect("send reload");

        let result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("wait_for_exit_signal should return after reload signal")
            .expect("task should not panic")
            .expect("signal handler should not error");
        assert_eq!(result, DaemonExit::Reload);
    }

    #[tokio::test]
    async fn registry_gateway_starter_can_trigger_daemon_reload() {
        use tokio::time::{Duration, timeout};

        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let expected_data_dir = config.data_dir.clone();
        let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut registry = DaemonRegistry::new();
        registry.register_gateway(Box::new(
            move |host, port, config, event_tx, reload_tx, tui_registry| {
                let seen_tx = seen_tx.clone();
                Box::pin(async move {
                    let has_event_tx = event_tx.is_some();
                    let has_reload_tx = reload_tx.is_some();
                    let has_tui_registry = tui_registry.is_some();
                    seen_tx
                        .send((
                            host,
                            port,
                            config.data_dir.clone(),
                            has_event_tx,
                            has_reload_tx,
                            has_tui_registry,
                        ))
                        .expect("record gateway starter inputs");
                    reload_tx
                        .expect("daemon should pass reload sender to gateway starter")
                        .send(true)
                        .expect("send reload signal");
                    std::future::pending::<Result<()>>().await
                })
            },
        ));

        let exit = timeout(
            Duration::from_secs(2),
            run(config, "127.0.0.1".to_string(), 4242, registry, false),
        )
        .await
        .expect("daemon should return after gateway-triggered reload")
        .expect("daemon run should succeed");

        assert_eq!(exit, DaemonExit::Reload);
        let (host, port, data_dir, has_event_tx, has_reload_tx, has_tui_registry) = seen_rx
            .try_recv()
            .expect("gateway starter should record its daemon inputs");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 4242);
        assert_eq!(data_dir, expected_data_dir);
        assert!(has_event_tx);
        assert!(has_reload_tx);
        assert!(has_tui_registry);
    }

    #[tokio::test]
    async fn ephemeral_does_not_exit_before_client_connects() {
        use tokio::time::{Duration, timeout};

        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(false);
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handle = zeroclaw_spawn::spawn!(wait_for_exit_signal(reload_rx, true, count));

        // No clients ever connect — should NOT shut down.
        let result = timeout(Duration::from_millis(500), handle).await;
        assert!(
            result.is_err(),
            "ephemeral daemon should not exit before any client connects"
        );
    }

    #[tokio::test]
    async fn ephemeral_exits_after_client_disconnects() {
        use std::sync::atomic::Ordering;
        use tokio::time::{Duration, timeout};

        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(false);
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count2 = count.clone();
        let handle = zeroclaw_spawn::spawn!(wait_for_exit_signal(reload_rx, true, count2));

        // Simulate client connect then disconnect.
        count.store(1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        count.store(0, Ordering::Relaxed);

        // Should exit within grace period + buffer.
        let result = timeout(Duration::from_secs(EPHEMERAL_GRACE_SECS + 5), handle)
            .await
            .expect("ephemeral daemon should shut down after last client disconnects")
            .expect("task should not panic")
            .expect("signal handler should not error");
        assert_eq!(result, DaemonExit::Shutdown);
    }

    #[tokio::test]
    async fn ephemeral_grace_period_resets_on_reconnect() {
        use std::sync::atomic::Ordering;
        use tokio::time::{Duration, timeout};

        let (_reload_tx, reload_rx) = tokio::sync::watch::channel(false);
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count2 = count.clone();
        let mut handle = zeroclaw_spawn::spawn!(wait_for_exit_signal(reload_rx, true, count2));

        // Client connects, disconnects.
        count.store(1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        count.store(0, Ordering::Relaxed);

        // Reconnect partway through the grace period — must be strictly
        // less than EPHEMERAL_GRACE_SECS so the daemon hasn't already
        // exited. With the 1s grace window we sleep ~200ms.
        tokio::time::sleep(Duration::from_millis(200)).await;
        count.store(1, Ordering::Relaxed);

        // Should NOT shut down while client is connected.
        let result = timeout(Duration::from_millis(500), &mut handle).await;
        assert!(
            result.is_err(),
            "ephemeral daemon should not exit while client is connected"
        );

        // Disconnect again — should eventually shut down.
        count.store(0, Ordering::Relaxed);
        let result = timeout(Duration::from_secs(EPHEMERAL_GRACE_SECS + 5), handle)
            .await
            .expect("ephemeral daemon should shut down after second disconnect")
            .expect("task should not panic")
            .expect("signal handler should not error");
        assert_eq!(result, DaemonExit::Shutdown);
    }
}
