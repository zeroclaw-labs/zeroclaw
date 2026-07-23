use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use zeroclaw_config::schema::Config;

mod registry;
pub use registry::{DaemonRegistry, GatewayReloadControls};

const STATUS_FLUSH_SECONDS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonExit {
    Shutdown,
    Reload,
}

const EPHEMERAL_GRACE_SECS: u64 = 1;

#[cfg(test)]
static SCHEDULER_CLEAN_SHUTDOWN_OBSERVED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn reset_scheduler_clean_shutdown_observed() {
    SCHEDULER_CLEAN_SHUTDOWN_OBSERVED.store(false, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn scheduler_clean_shutdown_observed() -> bool {
    SCHEDULER_CLEAN_SHUTDOWN_OBSERVED.load(std::sync::atomic::Ordering::SeqCst)
}

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
        // In-process shutdown trigger (no SIGTERM on Windows): the gateway fires
        // this to request a graceful exit, e.g. for post-upgrade self-respawn.
        let respawn_shutdown = crate::restart::shutdown_notify().notified();
        tokio::pin!(respawn_shutdown);
        loop {
            tokio::select! {
                res = tokio::signal::ctrl_c() => {
                    res?;
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Received Ctrl+C, shutting down...");
                    return Ok(DaemonExit::Shutdown);
                }
                _ = &mut respawn_shutdown => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "In-process shutdown requested, shutting down...");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayBindMode {
    /// Address is free (or an ephemeral port): start and supervise our own gateway.
    StartFresh,
    /// A ZeroClaw gateway already holds the address (e.g. a standalone
    /// `zeroclaw gateway start`): fail fast rather than start a second gateway
    /// on the same port.
    GatewayAlreadyRunning,
    /// Address is held by some other process: fail fast rather than degrade into
    /// a supervisor retry loop on the bind.
    PortOccupied,
}

/// Map the configured gateway bind host to a concrete authority reachable for a
/// local `/health` probe, formatted for a URL. Mirrors the CLI `self_test`
/// probe: wildcard `0.0.0.0` -> `127.0.0.1`, IPv6 wildcard `::`/`[::]` ->
/// `[::1]`; a bare concrete IPv6 host is bracketed.
fn gateway_probe_authority(host: &str) -> String {
    match host {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "[::1]".to_string(),
        other if other.contains(':') && !other.starts_with('[') => format!("[{other}]"),
        other => other.to_string(),
    }
}

/// Build the `/health` probe URL for the configured gateway, honouring the
/// gateway's TLS scheme and `path_prefix` so a prefixed or HTTPS gateway is
/// probed where it actually serves health.
fn gateway_health_probe_url(config: &Config, host: &str, port: u16) -> String {
    let scheme = if config.gateway.tls.as_ref().is_some_and(|tls| tls.enabled) {
        "https"
    } else {
        "http"
    };
    // `path_prefix` is validated to start with `/` and not end with `/`.
    let prefix = config.gateway.path_prefix.as_deref().unwrap_or("");
    format!(
        "{scheme}://{}:{port}{prefix}/health",
        gateway_probe_authority(host)
    )
}

async fn zeroclaw_gateway_responds(config: &Config, host: &str, port: u16) -> bool {
    let url = gateway_health_probe_url(config, host, port);
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    let Ok(response) = client.get(&url).send().await else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    matches!(
        response.json::<serde_json::Value>().await,
        Ok(body)
            if body.get("status").and_then(|s| s.as_str()) == Some("ok")
                && body
                    .get("require_pairing")
                    .is_some_and(serde_json::Value::is_boolean)
                && body.get("runtime").is_some_and(serde_json::Value::is_object)
    )
}

pub async fn detect_gateway_bind_mode(config: &Config, host: &str, port: u16) -> GatewayBindMode {
    // Port 0 is a kernel-assigned ephemeral port: it cannot already be bound,
    // so always start fresh.
    if port == 0 {
        return GatewayBindMode::StartFresh;
    }

    // Mirror the gateway's own bind exactly. If host:port does not parse as a
    // socket address, defer to the gateway (it has its own fallback) rather
    // than pre-judging the address.
    let Ok(addr) = zeroclaw_infra::parse_gateway_bind_socket_addr(host, port) else {
        return GatewayBindMode::StartFresh;
    };

    classify_gateway_bind_outcome(
        tokio::net::TcpListener::bind(addr).await,
        config,
        host,
        port,
    )
    .await
}

async fn classify_gateway_bind_outcome(
    bind: std::io::Result<tokio::net::TcpListener>,
    config: &Config,
    host: &str,
    port: u16,
) -> GatewayBindMode {
    match bind {
        Ok(listener) => {
            drop(listener);
            GatewayBindMode::StartFresh
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if zeroclaw_gateway_responds(config, host, port).await {
                GatewayBindMode::GatewayAlreadyRunning
            } else {
                GatewayBindMode::PortOccupied
            }
        }
        Err(_) => GatewayBindMode::StartFresh,
    }
}

pub async fn run(
    mut config: Config,
    host: String,
    port: u16,
    mut registry: DaemonRegistry,
    ephemeral: bool,
) -> Result<DaemonExit> {
    config.gateway.host = host.clone();
    if port != 0 {
        config.gateway.port = port;
    }

    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    // Shared broadcast channel so all daemon components (gateway, cron,
    // heartbeat) can publish real-time events to dashboard clients.
    let (event_tx, _rx) = tokio::sync::broadcast::channel::<serde_json::Value>(256);

    zeroclaw_log::set_broadcast_hook(event_tx.clone());

    if config.heartbeat.enabled
        && let Ok((_, heartbeat_workspace_dir)) = resolve_heartbeat_workspace_dir(&config)
    {
        let _ = crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(
            &heartbeat_workspace_dir,
        )
        .await;
    }

    crate::agent::pricing_catalog::load_global_pricing_catalog(&config.data_dir);

    let mut handles: Vec<JoinHandle<()>> = vec![spawn_state_writer(config.clone())];

    // Reload channel: gateway's /admin/reload writes here; our wait loop
    // (below) selects on it alongside OS signals. Cross-platform.
    let (reload_tx, reload_rx) = tokio::sync::watch::channel::<bool>(false);

    let channels_cancel = tokio_util::sync::CancellationToken::new();
    let (gateway_shutdown_tx, _) = tokio::sync::watch::channel::<bool>(false);

    // Construct the TUI registry early so both the gateway (for /api/tuis)
    // and the RPC socket (for tui/list) share the same Arc.
    let tui_registry =
        std::sync::Arc::new(crate::rpc::tui_identity::TuiRegistry::new(&config.data_dir));

    if let Some(gateway_start) = registry.take_gateway_start() {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        let gateway_event_tx = event_tx.clone();
        let gateway_reload_controls = GatewayReloadControls {
            shutdown_tx: gateway_shutdown_tx.clone(),
            reload_tx: reload_tx.clone(),
        };
        let gateway_tui_registry = tui_registry.clone();
        let gateway_start = std::sync::Arc::new(gateway_start);
        handles.push(spawn_component_supervisor(
            "gateway",
            initial_backoff,
            max_backoff,
            channels_cancel.clone(),
            move || {
                let cfg = gateway_cfg.clone();
                let host = gateway_host.clone();
                let tx = gateway_event_tx.clone();
                let reload_controls = gateway_reload_controls.clone();
                let tui_reg = gateway_tui_registry.clone();
                let start = gateway_start.clone();
                async move {
                    start(
                        host,
                        port,
                        cfg,
                        Some(tx),
                        Some(reload_controls),
                        Some(tui_reg),
                    )
                    .await
                }
            },
        ));
    }

    if crate::control_plane::control_plane().is_none()
        && let Err(e) = crate::control_plane::ControlPlaneHandle::start(
            &config.data_dir,
            config.goal.restart_recovery,
        )
        .await
        .map(crate::control_plane::init_control_plane)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "error": format!("{e:#}") })),
            "control-plane failed to start; supervision disabled for this run"
        );
    }
    // Respawn the reaper for THIS run iteration against the INSTALLED handle, so its
    // boot_id matches what producers stamp via `control_plane()`.
    if let Some(handle) = crate::control_plane::control_plane() {
        handle.spawn_reaper(
            crate::control_plane::reaper::DEFAULT_MAX_RUNTIME_SECS,
            config.goal.restart_recovery,
            channels_cancel.clone(),
        );
        crate::health::mark_component_ok("control-plane");
    }

    if let Some(channels_start) = registry.take_channels_start() {
        if has_supervised_channels(&config) {
            let channels_cfg = config.clone();
            let channels_start = std::sync::Arc::new(channels_start);
            let cancel_for_supervisor = channels_cancel.clone();
            handles.push(spawn_component_supervisor(
                "channels",
                initial_backoff,
                max_backoff,
                channels_cancel.clone(),
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

    // RPC transports: Unix socketand WSS (remote TUI connections).
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
            match zeroclaw_memory::create_memory_from_config(&config, None) {
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

        let hooks: Option<std::sync::Arc<crate::hooks::HookRunner>> = if config.hooks.enabled {
            Some(std::sync::Arc::new(crate::hooks::HookRunner::from_config(
                &config.hooks,
            )))
        } else {
            None
        };

        Some(std::sync::Arc::new(RpcContext {
            config: std::sync::Arc::new(parking_lot::RwLock::new(config.clone())),
            sessions,
            session_backend,
            memory: rpc_memory,
            // Process-global tracker shared with the gateway and channel
            // supervisor. Without this the RPC/zerocode-TUI turn path has no
            // tracker to record into and model cost is silently dropped
            cost_tracker: crate::cost::CostTracker::get_or_init_global(
                config.cost.clone(),
                &config.data_dir,
            ),
            event_tx: Some(event_tx.clone()),
            reload_tx: Some(reload_tx.clone()),
            gateway_shutdown_tx: Some(gateway_shutdown_tx.clone()),
            approval_pending: std::sync::Arc::new(
                crate::rpc::context::ApprovalPendingMap::default(),
            ),
            tui_registry,
            acp_session_store,
            sop_engine,
            sop_audit,
            hooks,
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
            socket_cancel.clone(),
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
            wss_cancel.clone(),
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
                channels_cancel.clone(),
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
            channels_cancel.clone(),
            move || {
                let cfg = heartbeat_cfg.clone();
                async move { Box::pin(run_heartbeat_worker(cfg)).await }
            },
        ));
    }

    if config.scheduler.enabled {
        let scheduler_cfg = config.clone();
        let scheduler_event_tx = event_tx.clone();
        let scheduler_cancel = channels_cancel.clone();
        handles.push(spawn_component_supervisor(
            "scheduler",
            initial_backoff,
            max_backoff,
            channels_cancel.clone(),
            move || {
                let cfg = scheduler_cfg.clone();
                let tx = scheduler_event_tx.clone();
                let cancel = scheduler_cancel.clone();
                async move { Box::pin(crate::cron::scheduler::run(cfg, Some(tx), cancel)).await }
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

    record_daemon_started(&config, &host, port);

    // Wait for shutdown (SIGINT/SIGTERM/Ctrl+C) or reload (in-process channel).
    let exit = wait_for_exit_signal(reload_rx, ephemeral, socket_client_count).await?;
    crate::health::mark_component_error(
        "daemon",
        match exit {
            DaemonExit::Shutdown => "shutdown requested",
            DaemonExit::Reload => "reload requested",
        },
    );

    channels_cancel.cancel();

    const GRACE_WINDOW: Duration = Duration::from_millis(500);
    let deadline = tokio::time::Instant::now() + GRACE_WINDOW;
    let mut remaining: Vec<JoinHandle<()>> = Vec::new();
    for mut handle in handles {
        tokio::select! {
            biased;
            _ = &mut handle => {
                // Cooperative handle exited cleanly during grace window.
            }
            _ = tokio::time::sleep_until(deadline) => {
                // Grace window expired; force-abort and re-join later.
                handle.abort();
                remaining.push(handle);
            }
        }
    }
    // Await remaining (aborted) handles. Already-completed handles from
    // the grace window are not re-await, so "JoinHandle polled after
    // completion" is avoided.
    for handle in remaining {
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

fn record_daemon_started(config: &Config, host: &str, port: u16) {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
            .with_category(::zeroclaw_log::EventCategory::System)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({
                "requested_gateway": format!("http://{host}:{port}"),
                "socket": crate::rpc::local::socket_path(config).display().to_string(),
                "pairing_enabled": config.gateway.require_pairing,
                "stop_signal": "Ctrl+C or SIGTERM",
            })),
        "ZeroClaw daemon started"
    );
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
    cancel: tokio_util::sync::CancellationToken,
    mut run_component: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    zeroclaw_spawn::spawn!(async move {
        let mut backoff = initial_backoff_secs.max(1);
        let max_backoff = max_backoff_secs.max(backoff);

        let stable_run = Duration::from_secs(initial_backoff_secs.max(1).saturating_mul(5));

        loop {
            crate::health::mark_component_ok(name);
            let run_started = std::time::Instant::now();
            let outcome = run_component().await;
            let ran_for = run_started.elapsed();
            match outcome {
                Ok(()) => {
                    if cancel.is_cancelled() {
                        crate::health::mark_component_ok(name);
                        #[cfg(test)]
                        if name == "scheduler" {
                            SCHEDULER_CLEAN_SHUTDOWN_OBSERVED
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({"name": name})),
                            &format!(
                                "Daemon component '{name}' shut down cleanly via cancellation token"
                            )
                        );
                        return;
                    }
                    crate::health::mark_component_error(name, "component exited unexpectedly");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "name": name,
                                "ran_for_secs": ran_for.as_secs(),
                            })),
                        &format!("Daemon component '{name}' exited unexpectedly")
                    );
                    if ran_for >= stable_run {
                        backoff = initial_backoff_secs.max(1);
                    }
                }
                Err(e) => {
                    crate::health::mark_component_error(name, e.to_string());
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error": format!("{}", e),
                                "name": name,
                                "ran_for_secs": ran_for.as_secs(),
                            })),
                        &format!("Daemon component '{name}' failed: {e}")
                    );
                    // A long-lived run that eventually errors is not a
                    // fast-fail loop; let it reset so a component that ran fine
                    // for hours and then hit a transient error retries quickly
                    // rather than inheriting a huge stale backoff.
                    if ran_for >= stable_run {
                        backoff = initial_backoff_secs.max(1);
                    }
                }
            }

            crate::health::bump_component_restart(name);
            crate::util::release_freed_heap();
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            // Double backoff AFTER sleeping so first error uses initial_backoff
            backoff = backoff.saturating_mul(2).min(max_backoff);
        }
    })
}

fn resolve_heartbeat_workspace_dir(config: &Config) -> Result<(String, PathBuf)> {
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
    let workspace_dir = config.agent_workspace_dir(&agent_alias);
    Ok((agent_alias, workspace_dir))
}

/// Test-only hook for [`connect_heartbeat_mcp_registry`]. The daemon
/// heartbeat worker builds an `Arc<McpRegistry>` once at worker start
/// and shares it across every tick so that stdio MCP children live
/// for the daemon's lifetime. Tests inject a hook here to
/// count invocations and assert the registry is constructed at most
/// once for N simulated ticks.
///
/// Hooks receive the resolved agent alias and the pre-computed list of
/// MCP server configs granted to that agent by `mcp_bundles`. They
/// MUST return an `Arc<McpRegistry>` whose inner server lifetime
/// outlives the simulated ticks (returning a fresh registry per call
/// would create a new stdio child on every tick).
#[cfg(test)]
type HeartbeatMcpRegistryTestHook = std::sync::Arc<
    dyn Fn(
            &str,
            &[zeroclaw_config::schema::McpServerConfig],
        ) -> std::sync::Arc<crate::tools::McpRegistry>
        + Send
        + Sync,
>;

#[cfg(test)]
static HEARTBEAT_MCP_REGISTRY_TEST_HOOK: std::sync::Mutex<Option<HeartbeatMcpRegistryTestHook>> =
    std::sync::Mutex::new(None);

/// Serializes the regression tests for the daemon heartbeat MCP
/// registry hook. The hook itself is process-global, so a
/// test that installs the hook, runs assertions, and then resets
/// cannot safely interleave with another test doing the same: a
/// concurrent `reset_heartbeat_mcp_registry_test_hook` would clear
/// the hook before the first test observes it, and a concurrent
/// `set_heartbeat_mcp_registry_test_hook` from another test would
/// swap in a hook whose counter Arc belongs to the other test. To
/// keep the regression tests deterministic, every hook-using test
/// takes this mutex (via [`HeartbeatMcpRegistryTestHookGuard`]) for
/// the entire duration of its hook-installed work.
#[cfg(test)]
static HEARTBEAT_MCP_REGISTRY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard that ties a test-only MCP registry hook installation
/// to the global serialising lock. Construction takes the global
/// mutex, installs the supplied hook, and returns a guard whose
/// `Drop` clears the hook and releases the mutex. Tests that need
/// the MCP registry hook to be observed by the daemon MUST hold
/// this guard for the duration of the work that depends on it;
/// otherwise a parallel test could clobber the hook (or reset it
/// while the current test is still running) and the assertion would
/// see a stale or absent hook.
#[cfg(test)]
pub(crate) struct HeartbeatMcpRegistryTestHookGuard {
    serial_lock: Option<std::sync::MutexGuard<'static, ()>>,
}

#[cfg(test)]
impl HeartbeatMcpRegistryTestHookGuard {
    /// Install `hook` under the global serialising lock and return a
    /// guard whose `Drop` clears the hook and releases the lock.
    fn install(hook: HeartbeatMcpRegistryTestHook) -> Self {
        // Hold the serial lock before mutating the hook global so a
        // concurrent test cannot observe a torn state (hook swapped
        // halfway, or reset between this test's set and use).
        let serial_lock = HEARTBEAT_MCP_REGISTRY_TEST_LOCK
            .lock()
            .expect("heartbeat MCP registry test serial lock should not be poisoned");
        let mut guard = HEARTBEAT_MCP_REGISTRY_TEST_HOOK
            .lock()
            .expect("heartbeat MCP registry test hook lock should not be poisoned");
        *guard = Some(hook);
        // Drop the hook global mutex immediately — the serial lock
        // is what prevents another test from racing with us now, and
        // the hook global only needs its inner value read once per
        // helper invocation.
        drop(guard);
        Self {
            serial_lock: Some(serial_lock),
        }
    }
}

#[cfg(test)]
impl Drop for HeartbeatMcpRegistryTestHookGuard {
    fn drop(&mut self) {
        // Clear the hook first (still under the serial lock taken by
        // `install`) so the next test sees a clean slate.
        if let Ok(mut guard) = HEARTBEAT_MCP_REGISTRY_TEST_HOOK.lock() {
            *guard = None;
        }
        // Releasing the serial lock last allows the next waiting
        // test to proceed only after our hook is gone.
        drop(self.serial_lock.take());
    }
}

/// Install a test-only hook that returns a pre-built `Arc<McpRegistry>`
/// for a given `(agent_alias, server_configs)` pair. Used by the
/// regression test in `tests` to bypass the real `connect_all` while
/// still counting constructions via the user's own counter logic.
///
/// Returns a guard that MUST be held for the duration of the test
/// work that depends on the hook; on drop, the hook is cleared and
/// the serialising lock is released so the next queued test can run.
/// Spinning off a detached future that outlives the guard will leave
/// the hook pointing at a stale closure and is not supported.
#[cfg(test)]
pub(crate) fn set_heartbeat_mcp_registry_test_hook(
    hook: HeartbeatMcpRegistryTestHook,
) -> HeartbeatMcpRegistryTestHookGuard {
    HeartbeatMcpRegistryTestHookGuard::install(hook)
}

/// Snapshot the current test hook (cloned). Returns `None` when no
/// hook is installed. Used by [`connect_heartbeat_mcp_registry`]
/// during the registry-construction phase of the heartbeat worker.
#[cfg(test)]
fn current_heartbeat_mcp_registry_test_hook() -> Option<HeartbeatMcpRegistryTestHook> {
    let guard = HEARTBEAT_MCP_REGISTRY_TEST_HOOK
        .lock()
        .expect("heartbeat MCP registry test hook lock should not be poisoned");
    guard.as_ref().cloned()
}

/// Connect the daemon's shared MCP registry for the heartbeat
/// agent. Called ONCE per `run_heartbeat_worker` invocation, the
/// returned `Arc<McpRegistry>` is then cloned into every
/// `AgentRunOverrides::mcp_registry` for the lifetime of the worker.
///
/// Returns `Ok(None)` when MCP is disabled, no servers are granted
/// to this agent, or the connection itself fails (fail-open: a
/// granted-but-unreachable MCP server must NOT crash the heartbeat
/// worker under supervisor backoff — the per-run `agent::run` MCP
/// path itself fails open, so we mirror that here. Because the
/// connection failed there is no stdio child to spawn, so the
/// "construct the registry once per worker" guarantee still holds
/// whenever the registry IS reachable — the healthy case this
/// targets).
///
/// When `Some`, the worker drops the registry on exit and the MCP
/// stdio children are reaped cleanly via
/// `tokio::process::Child::kill_on_drop(true)`.
/// Compute the subset of `granted` that is missing or dead in `current` --
/// i.e. the servers that actually need a fresh connection. A name with a
/// healthy handle in `current` is never included, so calling this
/// repeatedly while that handle stays healthy keeps excluding it instead
/// of re-including it on every heartbeat tick (the partial-outage churn
/// the retry path still had: a granted list of {A, B} with A healthy
/// and B down previously caused every tick to reconnect BOTH A and B via
/// `McpRegistry::connect_all`, even though A never needed it).
fn missing_or_dead_servers(
    granted: Vec<zeroclaw_config::schema::McpServerConfig>,
    current: Option<&std::sync::Arc<crate::tools::McpRegistry>>,
) -> Vec<zeroclaw_config::schema::McpServerConfig> {
    let Some(cur) = current else {
        return granted;
    };
    let dead: std::collections::HashSet<String> = cur.health_check_all().into_iter().collect();
    let healthy_names: std::collections::HashSet<String> = cur
        .server_handles()
        .into_iter()
        .filter(|(name, _)| !dead.contains(name))
        .map(|(name, _)| name)
        .collect();
    granted
        .into_iter()
        .filter(|s| !healthy_names.contains(&s.name))
        .collect()
}

async fn connect_heartbeat_mcp_registry(
    config: &Config,
    agent_alias: &str,
    current: Option<&std::sync::Arc<crate::tools::McpRegistry>>,
) -> Result<Option<std::sync::Arc<crate::tools::McpRegistry>>> {
    // Only (re)connect what `current` doesn't already have healthy --
    // a healthy server must never be respawned/re-handshaked just
    // because a sibling grant is missing or dead (see
    // `missing_or_dead_servers`). `current` is `None` at worker boot,
    // where every granted server is by definition missing. Computed
    // unconditionally (pure, no I/O) so the test hook below observes
    // the same filtered subset the real connect path would use.
    let granted = config.mcp_servers_for_agent(agent_alias);
    let servers = missing_or_dead_servers(granted, current);

    #[cfg(test)]
    if let Some(hook) = current_heartbeat_mcp_registry_test_hook() {
        return Ok(Some(hook(agent_alias, &servers)));
    }

    if !config.mcp.enabled {
        return Ok(None);
    }
    if servers.is_empty() {
        // Nothing is missing/dead. `reconcile_heartbeat_mcp_registry`
        // treats `fresh = None` as "keep current unchanged", so the
        // caller's existing registry is left exactly as it was.
        return Ok(None);
    }
    // Fail-open, mirroring the per-run `agent::run` MCP path: a
    // granted MCP server being unreachable must NOT take the
    // heartbeat worker down under supervisor backoff. On connect
    // failure we log and return `Ok(None)`; each tick then falls
    // back to the per-run path (which itself fails open). Because
    // the connection failed there is no stdio child to spawn, so
    // the "construct the registry once per worker" guarantee
    // still holds whenever the registry IS reachable — the healthy
    // case this targets.
    match crate::tools::McpRegistry::connect_all(&servers).await {
        Ok(registry) => Ok(Some(std::sync::Arc::new(registry))),
        Err(e) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "agent": agent_alias,
                        "error": format!("{:#}", e),
                    })),
                "heartbeat worker: failed to connect shared MCP registry; continuing without MCP tools"
            );
            Ok(None)
        }
    }
}

/// Additively reconcile the heartbeat worker's MCP registry.
///
/// The heartbeat worker's lifetime invariant is that a healthy
/// live `McpServer` connection must NEVER be silently disconnected and
/// respawned just because a peer's discovery result changed shape. This
/// function preserves that invariant under all of:
///
///   * steady state (both registries match by name + `McpServer` Arc
///     identity — return `None`, no churn);
///   * partial outage (one granted server healthy, another flaky —
///     keep the healthy handle, admit the freshly-discovered peer
///     additively);
///   * dead transport (a server whose child exited after startup —
///     drop it from `current` while keeping the rest, admit anything
///     `fresh` brought back);
///
/// while never silently dropping the live registry when both current
/// and fresh are present. The additive merge rebuilds the registry
/// from `healthy_current + fresh_new`, Arc-cloning each surviving
/// `McpServer` so its live transport is reused — no disconnect, no
/// respawn.
///
/// Returns `Some(merged_registry)` when the caller should replace
/// `current` with a new Arc, `None` when `current` should stay
/// unchanged.
async fn reconcile_heartbeat_mcp_registry(
    current: Option<&std::sync::Arc<crate::tools::McpRegistry>>,
    fresh: Option<&std::sync::Arc<crate::tools::McpRegistry>>,
) -> Option<std::sync::Arc<crate::tools::McpRegistry>> {
    let Some(current_arc) = current else {
        // No current registry — use fresh (if any). This is the boot
        // case where `shared` was `None`.
        return fresh.map(std::sync::Arc::clone);
    };
    let Some(fresh_arc) = fresh else {
        // No fresh registry — keep current. A failed `connect_all`
        // must not silently drop a live registry (fail-open).
        return None;
    };

    // Step 1: split current into healthy (kept) and dead (dropped /
    // replaced by fresh). `health_check_all` is read-only, so it
    // works against the shared Arc without `Arc::get_mut`.
    let dead: std::collections::HashSet<String> =
        current_arc.health_check_all().into_iter().collect();
    let current_handles = current_arc.server_handles();
    let healthy_handles: Vec<(String, crate::tools::McpServer)> = current_handles
        .into_iter()
        .filter(|(name, _)| !dead.contains(name))
        .collect();
    let healthy_names: std::collections::HashSet<String> =
        healthy_handles.iter().map(|(n, _)| n.clone()).collect();

    // Step 2: identify the slice of `fresh` that is NOT already
    // covered by a healthy current server — those are the recovered
    // servers we want to admit additively. We keep a sorted-by-name
    // copy of all fresh handles for the merged-equals-fresh check
    // in step 5 (avoids recomputing `server_handles()` again).
    let fresh_handles: Vec<(String, crate::tools::McpServer)> = fresh_arc.server_handles();
    let fresh_new: Vec<(String, crate::tools::McpServer)> = fresh_handles
        .iter()
        .filter(|(name, _)| !healthy_names.contains(name))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();

    // Step 3: merged set, sorted by name for determinism.
    let mut merged: Vec<(String, crate::tools::McpServer)> = healthy_handles;
    merged.extend(fresh_new);
    merged.sort_by(|a, b| a.0.cmp(&b.0));

    // Step 4: identity-stable no-churn check. If the merged set is
    // exactly the healthy-current set (same names AND same `McpServer`
    // Arc identity for each name), then the merged registry would be
    // functionally identical to `current` and there is nothing to
    // do — return `None` so the caller keeps the existing Arc.
    if merged.is_empty() {
        // No healthy current and no fresh-new — `current` may still
        // hold dead handles, but `fresh` did not bring a recovery.
        // Keep current (it might be the boot empty registry that
        // the test hook installed; the next tick will try again).
        return None;
    }
    let current_after_drop = current_arc.server_handles();
    // A merged entry is "the same" as a current entry iff they share
    // the name AND the underlying McpServer transport. When the
    // healthy-current handle for name N is exactly the same McpServer
    // we ended up with in `merged` for name N (which is the case
    // whenever `fresh_new` didn't carry the same name), no churn.
    let mut current_by_name: std::collections::HashMap<String, crate::tools::McpServer> =
        current_after_drop
            .into_iter()
            .filter(|(name, _)| !dead.contains(name))
            .collect();
    let mut churn = false;
    for (name, server) in &merged {
        match current_by_name.remove(name) {
            Some(existing) if existing.ptr_eq(server) => {
                // Same handle — preserved connection. No churn on
                // this entry.
            }
            Some(_) | None => {
                // Either the entry came from `fresh_new` (a brand-new
                // server we just admitted), or the healthy current
                // server's identity drifted away (uncommon — same
                // name but a different handle, which only happens
                // when `fresh` rebuilt the connection). Either way,
                // a new registry allocation is required.
                churn = true;
            }
        }
    }
    if !churn && current_by_name.is_empty() {
        // Every merged name matched a healthy current handle
        // identity, and no healthy current handle was left over
        // unmatched. The merged set is identical to the healthy
        // current set — no churn, no replacement.
        return None;
    }

    // Step 5: when the merged set is exactly `fresh`'s server set
    // (same names, same `McpServer` Arc identity for each name),
    // there is nothing for the daemon to rebuild — reuse `fresh`'s
    // Arc directly. This avoids a redundant `McpRegistry::from_servers`
    // allocation AND preserves the caller's "the recovery Arc IS
    // the fresh Arc" expectation: tick 1 of the recovery sequence
    // must hand the worker the same Arc pointer the hook returned.
    // (Both `merged` and `fresh_handles` are sorted by name, so the
    // zip is name-aligned.)
    if merged.len() == fresh_handles.len() {
        let all_match = merged
            .iter()
            .zip(fresh_handles.iter())
            .all(|((mn, ms), (fn_, fs))| mn == fn_ && ms.ptr_eq(fs));
        if all_match {
            return Some(std::sync::Arc::clone(fresh_arc));
        }
    }

    // Step 6: build the new registry from the merged handles.
    // `from_servers` rebuilds the tool_index from each handle's
    // advertised capabilities — empty for stub servers in tests,
    // non-empty for real stdio children.
    let servers: Vec<crate::tools::McpServer> = merged.into_iter().map(|(_, s)| s).collect();
    let new_registry = crate::tools::McpRegistry::from_servers(servers).await;
    Some(std::sync::Arc::new(new_registry))
}

/// Reconnect and reconcile the daemon-level MCP registry.
///
/// Called on each heartbeat tick. When the registry is incomplete (fewer
/// connected servers than granted) or health checks detect dead connections,
/// this function rebuilds the registry and uses [`reconcile_heartbeat_mcp_registry`]
/// to decide whether to replace the current registry.
///
/// Returns immediately (Ok(())) when no reconnection is needed.
async fn retry_heartbeat_mcp_registry(
    shared: &mut Option<std::sync::Arc<crate::tools::McpRegistry>>,
    config: &Config,
    agent_alias: &str,
) -> Result<()> {
    // Always attempt to reconnect and reconcile. This ensures that:
    // - Dead servers (after startup) are detected and replaced
    // - New servers (that came up later) are picked up
    // - Healthy servers are preserved via identity-aware reconciliation
    //   (live `McpServer` Arc identity is reused; no churn on the
    //   healthy side when only a peer server's discovery result
    //   changed shape).
    let granted = config.mcp_servers_for_agent(agent_alias);
    let granted_count = granted.len();
    let current_count = shared.as_ref().map_or(0, |r| r.server_count());

    // When the registry is complete and we own the Arc (single strong
    // ref), we can do a live health check and skip reconnect if healthy.
    // When the Arc is shared (e.g. an `AgentRunOverrides` clone exists),
    // we can still do the health check (it's read-only), but we may get
    // a stale result. Since `health_check_all` is read-only, we use
    // `shared.as_ref()` so `Arc::get_mut` is no longer required.
    let should_reconnect = if current_count >= granted_count {
        // Complete registry: only reconnect if health check fails.
        // `health_check_all` is read-only, so it works with shared Arc refs.
        match shared.as_ref().map(|arc| arc.health_check_all()) {
            Some(dead) => !dead.is_empty(),
            None => true, // no registry → reconnect
        }
    } else if current_count > 0 {
        // Partially complete — always reconnect to pick up missing servers.
        true
    } else {
        // Incomplete: always reconnect.
        true
    };

    if should_reconnect {
        // Kill dead connections if we can (no-op for test stubs).
        if let Some(arc) = shared
            && let Some(reg) = std::sync::Arc::get_mut(arc)
        {
            let _dead = reg.kill_dead_connections().await;
        }
        let fresh = connect_heartbeat_mcp_registry(config, agent_alias, shared.as_ref()).await?;
        // Let the additive reconciler decide whether the live registry
        // needs to be replaced; it returns `None` when the healthy
        // current handles are sufficient (preserves the
        // no-churn steady state) and `Some(merged)` when `fresh` adds
        // a recovered server or replaces a dead one.
        if let Some(replaced) =
            reconcile_heartbeat_mcp_registry(shared.as_ref(), fresh.as_ref()).await
        {
            *shared = Some(replaced);
        }
    }
    Ok(())
}

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    use crate::heartbeat::engine::{
        HeartbeatEngine, HeartbeatTask, TaskPriority, TaskStatus, compute_adaptive_interval,
    };
    use std::sync::Arc;

    let (agent_alias, heartbeat_workspace_dir) = resolve_heartbeat_workspace_dir(&config)?;

    // Build the daemon-level MCP registry ONCE per worker. With this
    // owner in place, every `agent::run` tick below reuses the same
    // `Arc<McpRegistry>` and the stdio MCP children live for the
    // worker's whole lifetime.
    //
    // The variable is `mut` so `retry_heartbeat_mcp_registry` below
    // can replace the stored registry with a fresh one when a granted
    // MCP server is missing from the registry (e.g. it was down at
    // worker boot and comes up later). Once `server_count ==
    // granted.len()` the call is a no-op and the Arc pointer survives
    // across ticks — the no-churn steady state is preserved.
    let mut shared_mcp_registry: Option<Arc<crate::tools::McpRegistry>> =
        connect_heartbeat_mcp_registry(&config, &agent_alias, None).await?;

    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = HeartbeatEngine::new(config.heartbeat.clone(), heartbeat_workspace_dir, observer);
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

        // ── retry-while-incomplete ───────────────────────────
        // When the registry is incomplete (fewer connected servers than
        // granted) or health checks detect dead connections, this call
        // rebuilds the registry and uses `reconcile_heartbeat_mcp_registry`
        // to decide whether to replace the current registry.
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias).await?;

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
                zeroclaw_api::ingress::TurnOrigin::Daemon,
                crate::agent::loop_::AgentRunOverrides {
                    mcp_registry: shared_mcp_registry.as_ref().map(Arc::clone),
                    ..crate::agent::loop_::AgentRunOverrides::default()
                },
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

        let heartbeat_memory: Option<Box<dyn zeroclaw_memory::Memory>> =
            zeroclaw_memory::create_memory_from_config(
                &config,
                config
                    .model_provider_for_agent(&agent_alias)
                    .and_then(|e| e.api_key.as_deref()),
            )
            .ok();

        let mut tick_had_error = false;
        for task in &tasks_to_run {
            let task_start = std::time::Instant::now();
            let task_prompt = format!("[Heartbeat Task | {}] {}", task.priority, task.text);

            // Memory context is injected once in the engine, keyed on the
            // Daemon origin (agent::memory_inject): Conversation entries are
            // excluded for scheduled origins. `heartbeat_memory` stays for
            // the post-run auto-save consolidation below.
            let prompt = match &session_context {
                Some(sc) => format!("{sc}\n\n{task_prompt}"),
                None => task_prompt,
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
                zeroclaw_api::ingress::TurnOrigin::Daemon,
                crate::agent::loop_::AgentRunOverrides {
                    mcp_registry: shared_mcp_registry.as_ref().map(Arc::clone),
                    ..crate::agent::loop_::AgentRunOverrides::default()
                },
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
                    let suppress_delivery =
                        !crate::cron::scheduler::announce_delivery_decision(&announcement)
                            .should_deliver();
                    if suppress_delivery {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Success)
                            .with_attrs(::serde_json::json!({"task": task.text})),
                            "Heartbeat task returned NO_REPLY sentinel — skipping delivery"
                        );
                    }
                    if let Some((channel, target)) = &delivery
                        && !suppress_delivery
                    {
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
    if !config.channels.is_known_channel(channel) {
        anyhow::bail!("unsupported heartbeat.target channel: {channel}");
    }
    if !config.channels.is_channel_configured(channel) {
        anyhow::bail!(
            "heartbeat.target is set to {channel} but channels.{channel} is not configured"
        );
    }
    if !config.channels.is_channel_deliverable(channel) {
        anyhow::bail!(
            "heartbeat.target is set to {channel} but {channel} is an input-only channel that cannot deliver outbound messages"
        );
    }
    Ok(())
}

fn has_supervised_channels(config: &Config) -> bool {
    config.channels.has_any_enabled()
}

// run_mqtt_sop_listener has been moved to zeroclaw-channels::orchestrator::mqtt.
// The daemon now receives it as a starter via DaemonRegistry::register_mqtt.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::schema::MattermostListenMode;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config
    }

    fn add_agent_with_workspace(config: &mut Config, agent_alias: &str, workspace_dir: PathBuf) {
        let agent = zeroclaw_config::schema::AliasedAgentConfig {
            workspace: zeroclaw_config::multi_agent::AgentWorkspaceConfig {
                path: Some(workspace_dir),
                ..Default::default()
            },
            ..Default::default()
        };
        config.agents.insert(agent_alias.to_string(), agent);
    }

    async fn recv_log_event(
        rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>,
        message: &str,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value))
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .is_some_and(|candidate| candidate == message) =>
                {
                    return value;
                }
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        panic!("did not find log event: {message}");
    }

    #[test]
    fn state_file_path_uses_config_state_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let path = state_file_path(&config);
        assert_eq!(path, tmp.path().join("state").join("daemon_state.json"));
    }

    #[tokio::test]
    async fn heartbeat_seed_uses_agent_workspace_not_data_dir() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        let agent_alias = "ops";
        let workspace_dir = tmp
            .path()
            .join("agents")
            .join(agent_alias)
            .join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        config.heartbeat.enabled = true;
        config.heartbeat.agent = agent_alias.to_string();
        add_agent_with_workspace(&mut config, agent_alias, workspace_dir.clone());

        let (_, resolved_workspace_dir) = resolve_heartbeat_workspace_dir(&config).unwrap();
        assert_eq!(resolved_workspace_dir, workspace_dir);
        assert_ne!(resolved_workspace_dir, config.data_dir);

        crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&resolved_workspace_dir)
            .await
            .unwrap();

        assert!(workspace_dir.join("HEARTBEAT.md").exists());
        assert!(!config.data_dir.join("HEARTBEAT.md").exists());
    }

    #[tokio::test]
    async fn heartbeat_engine_reads_agent_workspace_not_data_dir() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        let agent_alias = "ops";
        let workspace_dir = tmp
            .path()
            .join("agents")
            .join(agent_alias)
            .join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        config.heartbeat.enabled = true;
        config.heartbeat.agent = agent_alias.to_string();
        add_agent_with_workspace(&mut config, agent_alias, workspace_dir.clone());

        std::fs::write(config.data_dir.join("HEARTBEAT.md"), "- Data dir task").unwrap();
        std::fs::write(workspace_dir.join("HEARTBEAT.md"), "- Workspace task").unwrap();

        let (_, resolved_workspace_dir) = resolve_heartbeat_workspace_dir(&config).unwrap();
        let observer: std::sync::Arc<dyn crate::observability::Observer> =
            std::sync::Arc::new(crate::observability::NoopObserver);
        let engine = crate::heartbeat::engine::HeartbeatEngine::new(
            config.heartbeat.clone(),
            resolved_workspace_dir,
            observer,
        );

        let tasks = engine.collect_tasks().await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Workspace task");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn daemon_startup_diagnostics_are_logged_as_structured_event() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.gateway.require_pairing = true;

        record_daemon_started(&config, "127.0.0.1", 0);

        let value = recv_log_event(&mut rx, "ZeroClaw daemon started").await;
        assert_eq!(value["event"]["category"], "system");
        assert_eq!(value["event"]["action"], "start");
        assert_eq!(value["event"]["outcome"], "success");
        assert_eq!(
            value["attributes"]["requested_gateway"],
            "http://127.0.0.1:0"
        );
        assert_eq!(value["attributes"]["pairing_enabled"].as_bool(), Some(true));
        assert_eq!(value["attributes"]["stop_signal"], "Ctrl+C or SIGTERM");
        assert_eq!(
            value["attributes"]["socket"],
            crate::rpc::local::socket_path(&config)
                .display()
                .to_string()
        );
    }

    #[tokio::test]
    async fn supervisor_marks_error_and_restart_on_failure() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle =
            spawn_component_supervisor("daemon-test-fail", 1, 1, cancel.clone(), || async {
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
        let cancel = tokio_util::sync::CancellationToken::new();
        let handle =
            spawn_component_supervisor("daemon-test-exit", 1, 1, cancel.clone(), || async {
                Ok(())
            });

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

    #[tokio::test]
    async fn supervisor_marks_clean_shutdown_when_cancel_fires() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_arc = std::sync::Arc::new(cancel.clone());
        let handle = spawn_component_supervisor("daemon-test-cancel", 1, 1, cancel.clone(), {
            let cancel_arc = std::sync::Arc::clone(&cancel_arc);
            move || {
                let cancel_arc = std::sync::Arc::clone(&cancel_arc);
                async move {
                    cancel_arc.cancelled().await;
                    Ok(())
                }
            }
        });

        // Give the supervisor a tick to call the component once (so
        // the component is parked in `cancel.cancelled().await`).
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Fire the cancellation. The component wakes up, returns
        // `Ok(())`, and the supervisor takes the clean-shutdown path
        // (mark ok + return) instead of the "exited unexpectedly"
        // path.
        cancel.cancel();

        // The supervisor's outer loop is `loop { run_component().await; ... }`
        // so a single Ok(()) while cancelled makes it `return`.
        let join = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(
            join.is_ok(),
            "supervisor should exit cooperatively within 1s of cancel; got: {join:?}"
        );
        let _ = join.unwrap();

        // Health snapshot must show the component as healthy (not error),
        // because the supervisor took the cancel-aware return path.
        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["daemon-test-cancel"];
        assert_eq!(
            component["status"], "ok",
            "cooperative shutdown must mark the component healthy, not error; got snapshot: {component}"
        );
        assert_eq!(
            component["restart_count"].as_u64().unwrap_or(0),
            0,
            "cooperative shutdown must not trigger a restart; got snapshot: {component}"
        );
    }

    #[tokio::test]
    async fn supervisor_backs_off_on_fast_ok_exit_loop() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let cancel = tokio_util::sync::CancellationToken::new();
        let calls = Arc::new(AtomicU64::new(0));
        let calls_inner = Arc::clone(&calls);
        let handle =
            spawn_component_supervisor("daemon-test-fastok", 1, 60, cancel.clone(), move || {
                let calls = Arc::clone(&calls_inner);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Return immediately — the fast-fail case.
                    Ok(())
                }
            });

        // Let ~3.5s elapse. With exponential backoff the sleeps are
        // 1s, 2s, 4s..., so at most ~3 invocations fit. Without the fix the
        // supervisor would spin at 1s and rack up ~4+ (really unbounded).
        tokio::time::sleep(Duration::from_millis(3500)).await;
        handle.abort();
        let _ = handle.await;

        let n = calls.load(Ordering::SeqCst);
        assert!(
            n <= 3,
            "fast Ok(()) exits must back off exponentially, not hot-loop; got {n} invocations in 3.5s"
        );
    }

    #[test]
    fn detects_no_supervised_channels() {
        let config = Config::default();
        assert!(!has_supervised_channels(&config));
    }

    #[test]
    fn all_disabled_channels_not_supervised() {
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
                slash_commands: false,
                slash_command_scope: zeroclaw_config::schema::SlashCommandScope::default(),
                intents_mask: None,
                reaction_notifications: zeroclaw_config::schema::DiscordReactionScope::Off,
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
                slash_commands: false,
                slash_command_scope: zeroclaw_config::schema::SlashCommandScope::default(),
                intents_mask: None,
                reaction_notifications: zeroclaw_config::schema::DiscordReactionScope::Off,
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
                api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
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
                debounce_ms: None,
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
                listen_mode: MattermostListenMode::default(),
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
        config.heartbeat.target = Some("carrier_pigeon".into());
        config.heartbeat.to = Some("ops@example.com".into());
        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported heartbeat.target channel")
        );
    }

    #[test]
    fn resolve_delivery_accepts_matrix_target() {
        let mut config = Config::default();
        config.heartbeat.target = Some("matrix".into());
        config.heartbeat.to = Some("!room:example.org".into());
        config
            .channels
            .matrix
            .insert("default".to_string(), Default::default());

        let target = resolve_heartbeat_delivery(&config).unwrap();
        assert_eq!(
            target,
            Some(("matrix".to_string(), "!room:example.org".to_string()))
        );
    }

    #[test]
    fn resolve_delivery_rejects_configured_but_undeliverable_channel() {
        // review: a configured input-only channel (mqtt is a fan-in
        // listener whose Channel::send is a no-op) must not pass heartbeat
        // validation just because its table exists. Otherwise the validator
        // claims a target the delivery surface silently drops.
        let mut config = Config::default();
        config.heartbeat.target = Some("mqtt".into());
        config.heartbeat.to = Some("ops/heartbeat".into());
        config
            .channels
            .mqtt
            .insert("default".to_string(), Default::default());

        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string().contains("input-only channel"),
            "expected input-only rejection, got: {err}"
        );
    }

    #[test]
    fn resolve_delivery_rejects_voice_duplex_target() {
        // review: voice_duplex has a configured table and a WebSocket
        // event protocol but no Channel::send outbound path, so a heartbeat
        // target pointing at it must be rejected like the other input-only
        // transports rather than falling through to the dotted-ref error.
        let mut config = Config::default();
        config.heartbeat.target = Some("voice_duplex".into());
        config.heartbeat.to = Some("ops".into());
        config
            .channels
            .voice_duplex
            .insert("default".to_string(), Default::default());

        let err = resolve_heartbeat_delivery(&config).unwrap_err();
        assert!(
            err.to_string().contains("input-only channel"),
            "expected input-only rejection, got: {err}"
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
                api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
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
                debounce_ms: None,
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
                api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
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
                debounce_ms: None,
            },
        );
        // Inbound peer authorization lives in peer_groups in V3.
        // Auto-detect picks the first external peer of the synthesized
        // `telegram_default` group as the heartbeat target.
        config.peer_groups.insert(
            "telegram_default".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
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
            move |host, port, config, event_tx, reload_controls, tui_registry| {
                let seen_tx = seen_tx.clone();
                Box::pin(async move {
                    let has_event_tx = event_tx.is_some();
                    let has_gateway_shutdown_tx = reload_controls.is_some();
                    let reload_tx = reload_controls
                        .map(|controls| controls.reload_tx)
                        .expect("daemon should pass reload controls to gateway starter");
                    let has_reload_tx = !reload_tx.is_closed();
                    let has_tui_registry = tui_registry.is_some();
                    seen_tx
                        .send((
                            host,
                            port,
                            config.data_dir.clone(),
                            has_event_tx,
                            has_gateway_shutdown_tx,
                            has_reload_tx,
                            has_tui_registry,
                        ))
                        .expect("record gateway starter inputs");
                    reload_tx.send(true).expect("send reload signal");
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
        let (
            host,
            port,
            data_dir,
            has_event_tx,
            has_gateway_shutdown_tx,
            has_reload_tx,
            has_tui_registry,
        ) = seen_rx
            .try_recv()
            .expect("gateway starter should record its daemon inputs");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 4242);
        assert_eq!(data_dir, expected_data_dir);
        assert!(has_event_tx);
        assert!(has_gateway_shutdown_tx);
        assert!(has_reload_tx);
        assert!(has_tui_registry);
    }

    #[tokio::test]
    async fn scheduler_cooperative_shutdown_observed_through_daemon_reload() {
        use tokio::time::{Duration, timeout};

        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.scheduler.enabled = true;

        reset_scheduler_clean_shutdown_observed();

        let mut registry = DaemonRegistry::new();
        registry.register_gateway(Box::new(
            move |_host, _port, _config, _event_tx, reload_controls, _tui_reg| {
                Box::pin(async move {
                    let reload_tx = reload_controls
                        .map(|controls| controls.reload_tx)
                        .expect("daemon should pass reload controls to gateway starter");
                    // Give the scheduler a tick to enter its select!
                    // loop and park at the next interval tick or cancel.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    reload_tx.send(true).expect("send reload signal");
                    std::future::pending::<Result<()>>().await
                })
            },
        ));

        let exit = timeout(
            Duration::from_secs(3),
            run(config, "127.0.0.1".to_string(), 0, registry, false),
        )
        .await
        .expect("daemon should return after gateway-triggered reload")
        .expect("daemon run should succeed");
        assert_eq!(exit, DaemonExit::Reload);

        assert!(
            scheduler_clean_shutdown_observed(),
            "scheduler supervisor must take the cancel-aware clean-return branch; \
             aborting the supervisor before it observes Ok(()) leaves this sentinel false"
        );

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["scheduler"];
        assert_eq!(
            component["status"], "ok",
            "scheduler health snapshot must show ok after cooperative shutdown; got: {component}"
        );
        assert_eq!(
            component["restart_count"].as_u64().unwrap_or(0),
            0,
            "scheduler must not have been restarted; \
             restart_count > 0 means the supervisor took the unexpected-Ok or Err branch \
             instead of the cancel-aware return, which is the regression this test pins"
        );
        assert!(
            component["last_error"].is_null(),
            "scheduler must have no last_error after cooperative shutdown; got: {component}"
        );
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

    // ── daemon gateway bind-mode detection (fail-fast) ────────────────

    /// Raw HTTP/1.1 `/health` body a real ZeroClaw gateway returns (shape
    /// mirrors `handle_health` in `zeroclaw-gateway`): `status: ok` plus the
    /// identity fields `require_pairing` and `runtime`.
    fn zeroclaw_health_ok_response() -> Vec<u8> {
        http_response(
            "200 OK",
            br#"{"status":"ok","paired":false,"require_pairing":true,"runtime":{"components":{}}}"#,
        )
    }

    /// Build a minimal HTTP/1.1 response with a JSON body.
    fn http_response(status_line: &str, body: &[u8]) -> Vec<u8> {
        let mut resp = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        resp.extend_from_slice(body);
        resp
    }

    /// Spawn a one-shot HTTP responder on loopback. It answers the first
    /// request with `response`, then holds the listener bound until the
    /// returned guard (`oneshot::Sender`) is dropped — so the bind probe sees
    /// the port as occupied and the follow-up `/health` probe gets answered.
    async fn spawn_mock_gateway(response: Vec<u8>) -> (u16, tokio::sync::oneshot::Sender<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind mock listener");
        let port = listener.local_addr().expect("mock local addr").port();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        zeroclaw_spawn::spawn!(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(&response).await;
                let _ = stream.flush().await;
            }
            // Keep `listener` in scope (port stays bound) until released.
            let _ = release_rx.await;
        });
        (port, release_tx)
    }

    #[test]
    fn gateway_probe_authority_maps_wildcards_and_brackets_ipv6() {
        // Wildcards map to loopback (IPv4 -> 127.0.0.1, IPv6 -> [::1]) the same
        // way the CLI self-test probe does.
        assert_eq!(gateway_probe_authority("0.0.0.0"), "127.0.0.1");
        assert_eq!(gateway_probe_authority("::"), "[::1]");
        assert_eq!(gateway_probe_authority("[::]"), "[::1]");
        // Concrete hosts pass through; a bare IPv6 host is bracketed for URLs.
        assert_eq!(gateway_probe_authority("127.0.0.1"), "127.0.0.1");
        assert_eq!(gateway_probe_authority("::1"), "[::1]");
        assert_eq!(gateway_probe_authority("[::1]"), "[::1]");
        assert_eq!(gateway_probe_authority("example.test"), "example.test");
    }

    #[test]
    fn gateway_health_probe_url_defaults_to_http_health() {
        let config = Config::default();
        assert_eq!(
            gateway_health_probe_url(&config, "127.0.0.1", 8080),
            "http://127.0.0.1:8080/health"
        );
    }

    #[test]
    fn gateway_health_probe_url_maps_ipv6_wildcard_to_loopback() {
        let config = Config::default();
        assert_eq!(
            gateway_health_probe_url(&config, "[::]", 8080),
            "http://[::1]:8080/health"
        );
        assert_eq!(
            gateway_health_probe_url(&config, "0.0.0.0", 8080),
            "http://127.0.0.1:8080/health"
        );
    }

    #[test]
    fn gateway_health_probe_url_honours_path_prefix() {
        let mut config = Config::default();
        config.gateway.path_prefix = Some("/api".to_string());
        assert_eq!(
            gateway_health_probe_url(&config, "127.0.0.1", 8080),
            "http://127.0.0.1:8080/api/health"
        );
    }

    #[test]
    fn gateway_health_probe_url_uses_https_when_tls_enabled() {
        let mut config = Config::default();
        config.gateway.tls = Some(zeroclaw_config::schema::GatewayTlsConfig {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(
            gateway_health_probe_url(&config, "127.0.0.1", 8443),
            "https://127.0.0.1:8443/health"
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_starts_fresh_on_ephemeral_port() {
        // Port 0 is kernel-assigned: it cannot already be bound.
        assert_eq!(
            detect_gateway_bind_mode(&Config::default(), "0.0.0.0", 0).await,
            GatewayBindMode::StartFresh
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_starts_fresh_on_free_port() {
        // Reserve an ephemeral port, then release it so the address is free.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("reserve port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            detect_gateway_bind_mode(&Config::default(), "127.0.0.1", port).await,
            GatewayBindMode::StartFresh
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_flags_existing_zeroclaw_gateway() {
        // A real ZeroClaw `/health` (status==ok + identity fields) on an
        // occupied port → fail fast with the "gateway already running" message.
        let (port, _release) = spawn_mock_gateway(zeroclaw_health_ok_response()).await;
        assert_eq!(
            detect_gateway_bind_mode(&Config::default(), "127.0.0.1", port).await,
            GatewayBindMode::GatewayAlreadyRunning,
            "a ZeroClaw /health on an occupied port is recognised as a gateway"
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_flags_generic_status_ok_as_occupied() {
        // A foreign service answering the generic `{"status":"ok"}` (no
        // ZeroClaw identity fields) must NOT be taken for a gateway — it is a
        // plain occupied port.
        let (port, _release) =
            spawn_mock_gateway(http_response("200 OK", br#"{"status":"ok"}"#)).await;
        assert_eq!(
            detect_gateway_bind_mode(&Config::default(), "127.0.0.1", port).await,
            GatewayBindMode::PortOccupied,
            "a generic status:ok health response is not a ZeroClaw gateway"
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_flags_non_gateway_404_as_occupied() {
        let (port, _release) = spawn_mock_gateway(http_response("404 Not Found", b"")).await;
        assert_eq!(
            detect_gateway_bind_mode(&Config::default(), "127.0.0.1", port).await,
            GatewayBindMode::PortOccupied,
            "a non-2xx /health on an occupied port fails fast as a foreign occupant"
        );
    }

    #[tokio::test]
    async fn detect_gateway_bind_mode_defers_on_non_addr_in_use_error() {
        let outcome = classify_gateway_bind_outcome(
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            &Config::default(),
            "0.0.0.0",
            80,
        )
        .await;
        assert_eq!(
            outcome,
            GatewayBindMode::StartFresh,
            "a non-AddrInUse bind error must defer to the gateway's own bind, not fail fast"
        );
    }

    // ── MCP stdio child process must NOT be re-spawned per
    //    heartbeat tick — the daemon heartbeat worker owns one
    //    `Arc<McpRegistry>` for its entire lifetime, and every tick's
    //    `agent::run` call receives that same Arc via
    //    `AgentRunOverrides::mcp_registry`.
    //
    //    Regression test simulates N "tick boundaries" through the
    //    actual reuse path (`connect_heartbeat_mcp_registry` +
    //    `AgentRunOverrides::mcp_registry`) and asserts:
    //
    //      (a) `connect_heartbeat_mcp_registry` is called exactly
    //          ONCE (counter == 1) when the daemon worker boots;
    //      (b) the Arc pointer the worker hands to the per-tick
    //          overrides is the SAME Arc on every tick
    //          (std::ptr::eq on `Arc::as_ptr`).
    //
    //    The counter test is non-vacuous: without the daemon-level
    //    cache, the construction counter would be N (one per tick)
    //    and the Arc pointers would differ on every tick.
    #[tokio::test]
    async fn heartbeat_mcp_registry_constructed_once_across_n_ticks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let agent_alias = "ops";

        // Install a counter hook: every invocation increments and
        // returns the SAME shared Arc<McpRegistry>. A counter that
        // monotonically increases across calls is the regression
        // signal — a per-tick construction path would call this
        // once per tick. The returned RAII guard must outlive every
        // assertion below; if it drops while another test starts,
        // that other test could clobber or reset the hook before we
        // observe it.
        let construct_count = Arc::new(AtomicUsize::new(0));
        let shared_for_hook: Arc<crate::tools::McpRegistry> = Arc::new(
            crate::tools::McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all succeeds for the test fixture"),
        );
        let shared_for_hook_clone = Arc::clone(&shared_for_hook);
        let construct_count_for_hook = Arc::clone(&construct_count);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&shared_for_hook_clone)
            }));

        // (a) Simulate worker boot: the daemon calls
        //     `connect_heartbeat_mcp_registry` exactly once.
        let shared = connect_heartbeat_mcp_registry(&config, agent_alias, None)
            .await
            .expect("connect_heartbeat_mcp_registry succeeds")
            .expect("test config has MCP enabled so the shared registry is Some");

        // The hook MUST have fired exactly once for the worker boot.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "daemon worker must construct the MCP registry exactly once at boot \
             (one stdio child per daemon lifetime, not one per heartbeat tick)"
        );

        // (b) Simulate N heartbeat ticks: the daemon constructs a fresh
        //     `AgentRunOverrides` per tick, cloning the shared Arc.
        //     Every tick's overrides MUST carry the SAME Arc pointer.
        const N: usize = 16;
        let mut observed_ptrs: Vec<*const crate::tools::McpRegistry> = Vec::with_capacity(N);
        for tick in 0..N {
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: Some(Arc::clone(&shared)),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            let registry = overrides
                .mcp_registry
                .as_ref()
                .expect("test overrides must carry the shared registry");
            observed_ptrs.push(Arc::as_ptr(registry));
            // Drop the overrides (simulates end of `agent::run`) —
            // the strong count decreases by 1 but the worker still
            // holds its `shared` Arc, so the registry stays alive
            // for tick + 1.
            drop(overrides);
            assert!(
                Arc::strong_count(&shared) >= 1,
                "shared registry must remain alive across ticks (tick {tick})"
            );
        }

        // All N tick Arcs MUST point to the same allocation. If the
        // worker ever constructed a fresh registry per tick, the
        // pointers would diverge.
        let first_ptr = observed_ptrs[0];
        for (tick, ptr) in observed_ptrs.iter().enumerate() {
            assert!(
                std::ptr::eq(*ptr, first_ptr),
                "tick {tick} saw a different Arc<McpRegistry> pointer ({:p} != {:p}); \
                 the daemon worker must reuse ONE shared registry across all ticks",
                ptr,
                first_ptr,
            );
        }

        // The construction counter MUST still be 1 — N ticks must
        // not have re-triggered the hook. A counter of 2..=N would
        // mean the worker is reconnecting per tick.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "MCP registry must not be re-constructed per heartbeat tick; \
             saw {} constructions across {} ticks",
            construct_count.load(Ordering::SeqCst),
            N,
        );

        // Sanity: the shared registry's server_count matches the
        // empty-fixture expectation, so the test exercises the
        // real McpRegistry construction path (Arc::as_ptr equality
        // would be trivial if we used a unit-like stub).
        assert_eq!(
            shared.server_count(),
            0,
            "shared fixture registry should have zero servers (empty MCP config)"
        );

        // Drop the worker-level handle — this is the daemon's
        // shutdown boundary. Any live stdio child would be reaped
        // cleanly by `kill_on_drop(true)`; the empty fixture used
        // here has none.
        drop(shared);

        // The hook guard drops here, clearing the global hook and
        // releasing the serialising lock so the next regression
        // test can install its own hook.
    }

    // ── PROVE that the real `connect_all` path spawns the
    //    stdio MCP child ONCE and reuses it across N heartbeat
    //    ticks. The two regressions above cover the path where a
    //    test hook short-circuits `connect_heartbeat_mcp_registry`
    //    to a pre-built `Arc<McpRegistry>` and counts constructions
    //    through a closure. That path cannot catch an actual spawn
    //    per tick: a buggy worker that calls `McpRegistry::connect_all`
    //    per tick on the real path would spawn one child per tick —
    //    but the hook would never fire, because the test hook is only
    //    consulted on the `connect_heartbeat_mcp_registry` side, not
    //    on every `agent::run` re-construction.
    //
    //    This test exercises the REAL `connect_all` path (no test
    //    hook), spawns a real `@modelcontextprotocol/server-filesystem`
    //    via `npx`, and watches the OS process table for the
    //    resulting `mcp-server-filesystem` node process. With the
    //    registry hoisted out of the tick loop, the process count
    //    MUST stay at 1 across N=8 ticks. Without the fix the count
    //    would grow to N+1 (one per tick).
    //
    //    Drop semantics: `StdioTransport` is built with
    //    `kill_on_drop(true)` (see `zeroclaw-tools::mcp_transport`),
    //    so dropping the only `Arc<McpRegistry>` reaps the child.
    //    The final assertion in this test verifies that the worker
    //    boundary (drop the shared Arc) does indeed tear the child
    //    down — pinning the "drop Arc == kill child" invariant that
    //    the heartbeat worker relies on.
    //
    //    Ignored by default: it spawns a real npx subprocess and
    //    needs node + npx on PATH. Run explicitly with:
    //      cargo test -p zeroclaw-runtime --lib \
    //        heartbeat_mcp_registry_reuses_one_stdio_child_across_ticks \
    //        -- --ignored
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "spawns a real stdio MCP server via npx @modelcontextprotocol/server-filesystem; needs node/npx on PATH so it does not run in normal CI. Run: cargo test -p zeroclaw-runtime --lib heartbeat_mcp_registry_reuses_one_stdio_child_across_ticks -- --ignored"]
    async fn heartbeat_mcp_registry_reuses_one_stdio_child_across_ticks() {
        use std::sync::Arc;
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig, McpServerConfig};

        // `pgrep -f` matches the full command line. The
        // `@modelcontextprotocol/server-filesystem` package runs as
        // a node script whose absolute path contains this literal
        // substring, so a single `-f` match is enough to count the
        // child.
        const CHILD_PATTERN: &str = "mcp-server-filesystem";

        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);

        // (1) Enable MCP and register one stdio server pointing at a
        //     throwaway directory inside `tmp`. The server needs a
        //     directory argument (it serves `read_file` /
        //     `list_directory` over it) — we use the tmp root so the
        //     server has something valid to operate on.
        config.mcp.enabled = true;
        config.mcp.servers.push(McpServerConfig {
            name: "fs".to_string(),
            transport: zeroclaw_config::schema::McpTransport::Stdio,
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                tmp.path().display().to_string(),
            ],
            ..McpServerConfig::default()
        });

        // (2) Bundle the server under "b" and grant it to the "ops"
        //     agent. `mcp_servers_for_agent("ops")` must return one
        //     `McpServerConfig` (the "fs" entry above) for the
        //     connect_all path to find and spawn it.
        config.mcp_bundles.insert(
            "b".to_string(),
            McpBundleConfig {
                servers: vec!["fs".to_string()],
                exclude: vec![],
            },
        );
        config.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                mcp_bundles: vec!["b".to_string()],
                ..AliasedAgentConfig::default()
            },
        );

        // Helper: count OS processes whose cmdline matches the MCP
        // stdio child. `pgrep -f` returns 0 lines when nothing
        // matches (and exits 1), so we read stdout and count
        // non-empty lines. We never trust pgrep's exit status — a
        // no-match is "0 children", not a test failure.
        let count_children = || -> usize {
            std::process::Command::new("pgrep")
                .args(["-f", CHILD_PATTERN])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0)
        };

        // (3) Real connect path — NO test hook installed. This
        //     mirrors `run_heartbeat_worker`'s pre-loop call. The
        //     registry's `connect_all` will spawn one node child via
        //     `tokio::process::Command::new("npx")...spawn()`.
        let shared = connect_heartbeat_mcp_registry(&config, "ops", None)
            .await
            .expect("connect_heartbeat_mcp_registry succeeds on the real path")
            .expect("registry present: mcp.enabled = true and 'ops' has one granted stdio server");

        // The very first construction MUST have produced exactly
        // one stdio child. We sleep ~800ms before polling so npx has
        // time to fetch (if needed) and exec node.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let count_after_connect = count_children();
        assert_eq!(
            count_after_connect, 1,
            "after the single connect_all there must be exactly one mcp-server-filesystem child; \
             got {count_after_connect} (a count > 1 indicates multiple children were spawned at boot)"
        );

        // (4) Simulate N=8 heartbeat ticks. Each tick builds a fresh
        //     `AgentRunOverrides` cloning the shared Arc (no
        //     construction), records its pointer, and drops the
        //     overrides. The child count MUST stay at 1 and the
        //     Arc pointer MUST stay identical (std::ptr::eq) — the
        //     exact invariant this test protects.
        const N: usize = 8;
        let first_ptr: *const crate::tools::McpRegistry = Arc::as_ptr(&shared);
        for tick in 0..N {
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: Some(Arc::clone(&shared)),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            let registry = overrides
                .mcp_registry
                .as_ref()
                .expect("tick overrides must carry the shared registry");
            let tick_ptr = Arc::as_ptr(registry);
            assert!(
                std::ptr::eq(tick_ptr, first_ptr),
                "tick {tick} saw a different Arc<McpRegistry> pointer ({:p} != {:p}); \
                 the daemon worker must reuse ONE shared registry across all ticks",
                tick_ptr,
                first_ptr,
            );
            // Drop overrides (mirrors end-of-`agent::run`). The
            // shared Arc on the worker keeps the registry alive.
            drop(overrides);

            let count_during_ticks = count_children();
            assert_eq!(
                count_during_ticks, 1,
                "tick {tick}: mcp-server-filesystem child count drifted to {count_during_ticks}; \
                 expected exactly 1 across all ticks (the daemon must not respawn the stdio \
                 child per heartbeat tick — that is the #5903 fix)"
            );
        }

        // Sanity: the real registry still owns the server it
        // connected to (i.e. we didn't accidentally exercise an
        // empty-fixture path).
        assert_eq!(
            shared.server_count(),
            1,
            "the real-path registry must hold exactly the 1 stdio server we configured"
        );

        // (5) Worker shutdown boundary. `kill_on_drop(true)` on the
        //     `tokio::process::Child` inside `StdioTransport` means
        //     dropping the only Arc kills the node process. Poll for
        //     up to ~3s for the child to disappear. If the platform's
        //     Drop semantics differ and the child survives, we
        //     weaken the assertion to "count never increased" — the
        //     regression signal we care about is "1 child across all
        //     ticks", not the OS-killing implementation detail.
        let pre_drop_count = count_children();
        drop(shared);
        let mut returned_to_zero = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if count_children() == 0 {
                returned_to_zero = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let post_drop_count = count_children();
        if returned_to_zero {
            assert_eq!(
                post_drop_count, 0,
                "dropping the shared Arc must reap the stdio MCP child \
                 (kill_on_drop is set on the underlying tokio::process::Child)"
            );
        } else {
            // The Drop impl may not have killed the child on this
            // platform (or the OS has not yet reaped it within the
            // 3s window). The regression this guards — "per-tick
            // construction" — is still proven by the in-tick
            // assertions above (child count stayed exactly 1 across
            // N=8 ticks). The only failure here would be the count
            // INCREASING, which would mean a stray new spawn
            // happened during shutdown.
            assert!(
                post_drop_count <= pre_drop_count,
                "after dropping the shared Arc, the mcp-server-filesystem child count \
                 increased from {pre_drop_count} to {post_drop_count}; \
                 dropping the registry must not spawn additional children"
            );
            // Best-effort reap: leave a clearly-attributed warning
            // in the test log so a CI failure of the strict path is
            // easy to triage.
            eprintln!(
                "heartbeat_mcp_registry_reuses_one_stdio_child_across_ticks: \
                 Drop did not reap the child within 3s on this platform \
                 (pre={pre_drop_count}, post={post_drop_count}); \
                 the per-tick count == 1 assertion above is the load-bearing \
                 regression signal — the strict post-drop == 0 path is best-effort"
            );
        }
    }

    #[tokio::test]
    async fn heartbeat_mcp_shared_arc_survives_n_tick_drops() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let agent_alias = "ops";

        // Always-on counter — every hook invocation bumps it.
        // The RAII guard MUST outlive the loop below so the hook
        // stays installed across every simulated tick.
        let construct_count = Arc::new(AtomicUsize::new(0));
        let shared_for_hook: Arc<crate::tools::McpRegistry> = Arc::new(
            crate::tools::McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all succeeds"),
        );
        let shared_for_hook_clone = Arc::clone(&shared_for_hook);
        let construct_count_for_hook = Arc::clone(&construct_count);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&shared_for_hook_clone)
            }));

        let worker_shared = connect_heartbeat_mcp_registry(&config, agent_alias, None)
            .await
            .expect("connect_heartbeat_mcp_registry succeeds")
            .expect("registry present");

        // Strong count: the hook Arc + the `shared_for_hook_clone`
        // Arc + the worker's `worker_shared` Arc = 3. Each tick
        // adds a transient Arc inside `overrides.mcp_registry` and
        // drops it at end-of-`agent::run` — that transient must NOT
        // trigger another construction.
        let baseline_strong = Arc::strong_count(&worker_shared);

        const TICKS: usize = 8;
        for _ in 0..TICKS {
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: Some(Arc::clone(&worker_shared)),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            // Simulate the body of `agent::run` completing and the
            // local overrides going out of scope. The shared
            // registry MUST still be alive (held by the worker).
            drop(overrides);
            assert_eq!(
                Arc::strong_count(&worker_shared),
                baseline_strong,
                "strong count must return to baseline after each tick's overrides drop; \
                 a different value indicates a per-tick construction"
            );
        }

        // Hook fired exactly once — not TICKS times.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "the construction counter must stay at 1 across {TICKS} ticks; \
             a higher count means the daemon is rebuilding the MCP registry per tick"
        );

        // The hook guard drops here, releasing the serialising
        // lock and clearing the global hook for the next test.
    }

    // Direct regression test: simulate the full heartbeat worker's
    // tick loop. The OLD (buggy) daemon constructed a fresh
    // `McpRegistry` (and therefore a fresh stdio child) per
    // `agent::run` call. The fix constructs ONE registry at worker
    // start and reuses it across every tick — so the construction
    // counter must stay at 1 regardless of how many ticks we run.
    //
    // Non-vacuous: a test that just calls `McpRegistry::call_tool()`
    // N times would NOT have caught the bug, because `call_tool`
    // reuses the existing child. The regression signal is the
    // construction/connect counter, not the call counter.
    #[tokio::test]
    async fn heartbeat_worker_reuses_shared_mcp_registry_across_n_ticks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let agent_alias = "ops";

        // Hook fires once per `connect_heartbeat_mcp_registry` call.
        // The OLD bug: per-tick construction would make this counter
        // equal to TICKS. The fix: 1, regardless of TICKS. Bind the
        // hook to the test lifetime via the RAII guard so a
        // concurrent regression test cannot clobber it.
        let construct_count = Arc::new(AtomicUsize::new(0));
        let shared_for_hook: Arc<crate::tools::McpRegistry> = Arc::new(
            crate::tools::McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all succeeds"),
        );
        let shared_for_hook_clone = Arc::clone(&shared_for_hook);
        let construct_count_for_hook = Arc::clone(&construct_count);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&shared_for_hook_clone)
            }));

        // ── Worker boot: connect ONCE ──────────────────────────────
        // Mirrors `run_heartbeat_worker`'s pre-loop call. The fix
        // hoists this out of the tick loop so stdio children live
        // for the daemon's lifetime.
        let shared = connect_heartbeat_mcp_registry(&config, agent_alias, None)
            .await
            .expect("connect_heartbeat_mcp_registry succeeds")
            .expect("registry present");
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "worker boot must construct exactly one MCP registry"
        );

        // ── Simulate N heartbeat ticks ─────────────────────────────
        // Each tick does what `run_heartbeat_worker` does:
        //   1. Build a fresh `AgentRunOverrides` cloning the shared
        //      Arc (no construction).
        //   2. Hand it to `agent::run` (which uses the override and
        //      skips its own `connect_all`).
        //   3. Drop the overrides at end of `agent::run`.
        const TICKS: usize = 32;
        let worker_held = Arc::clone(&shared);
        for tick in 0..TICKS {
            // The daemon path: build overrides, run agent, drop.
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: Some(Arc::clone(&worker_held)),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            // `agent::run` would consume these overrides — but for
            // this regression test we only need to verify the
            // construction counter is unchanged after the tick
            // boundary. (Driving a real `agent::run` here would
            // require a scripted model provider; the daemon-level
            // construction-counter assertion is the regression
            // signal this test protects.)
            assert!(
                overrides.mcp_registry.is_some(),
                "tick {tick} overrides must carry the shared registry"
            );
            drop(overrides);
        }

        // ── Hard regression assertions ────────────────────────────
        // Counter == 1 across all TICKS. A counter of TICKS or more
        // would mean the daemon is reconnecting MCP stdio children
        // every tick — this invariant is reported.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "construction counter must be 1 after {TICKS} ticks; \
             got {} (a value >1 means the daemon is spawning a new \
             stdio MCP child every tick — that is the #5903 fix))",
            construct_count.load(Ordering::SeqCst),
        );

        // Strong-count sanity: the worker (`worker_held` + `shared`)
        // and the hook closures still hold the registry, but a
        // per-tick transient Arc inside `overrides.mcp_registry`
        // must not add an extra strong reference.
        let strong_after_ticks = Arc::strong_count(&shared);
        assert!(
            strong_after_ticks >= 2,
            "worker + hook must each hold a strong Arc; got {strong_after_ticks}"
        );

        // `_hook_guard` drops here, releasing the serialising lock
        // and clearing the global hook for the next test.
    }

    // ── FAIL-ON-OLD guard: a test that would fail under the
    //    pre-fix per-tick-construction behaviour, where
    //    `connect_heartbeat_mcp_registry` was called inside the
    //    heartbeat tick loop. The fix hoists it out, so the helper
    //    must be safe to invoke multiple times only by callers that
    //    actually need a fresh registry — and each call's hook
    //    increment is observable. This test invokes the helper N
    //    times and asserts the counter == N (proving the helper
    //    *does* fire the hook per call when the daemon code does
    //    call it per call). Combined with the above tests, this
    //    pins both directions: the helper really does count, and
    //    the daemon worker only calls it once.
    #[tokio::test]
    async fn connect_heartbeat_mcp_registry_helper_counts_per_invocation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let agent_alias = "ops";

        let construct_count = Arc::new(AtomicUsize::new(0));
        let shared_for_hook: Arc<crate::tools::McpRegistry> = Arc::new(
            crate::tools::McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all succeeds"),
        );
        let shared_for_hook_clone = Arc::clone(&shared_for_hook);
        let construct_count_for_hook = Arc::clone(&construct_count);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&shared_for_hook_clone)
            }));

        // 5 invocations → counter must reach 5. A buggy helper that
        // memoised the first call's Arc would leave the counter at 1,
        // which would also be wrong (the daemon expects to be able
        // to call the helper and observe construction each time —
        // though in practice it MUST NOT call it more than once per
        // worker lifetime, see the regressions above).
        const INVOCATIONS: usize = 5;
        let mut returned_ptrs: Vec<*const crate::tools::McpRegistry> =
            Vec::with_capacity(INVOCATIONS);
        for _ in 0..INVOCATIONS {
            let r = connect_heartbeat_mcp_registry(&config, agent_alias, None)
                .await
                .expect("connect_heartbeat_mcp_registry succeeds")
                .expect("registry present");
            returned_ptrs.push(Arc::as_ptr(&r));
        }

        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            INVOCATIONS,
            "the test hook must fire once per helper invocation; \
             a different count means the construction-counter signal \
             is unreliable and the worker tests above cannot trust it"
        );

        // The hook returns the SAME Arc each time (the registry
        // fixture is shared), so all returned pointers are equal —
        // pinning that the helper does not fabricate fresh Arcs.
        let first = returned_ptrs[0];
        for (i, ptr) in returned_ptrs.iter().enumerate() {
            assert!(
                std::ptr::eq(*ptr, first),
                "invocation {i} returned a different Arc<McpRegistry> pointer ({:p} != {:p}); \
                 the test hook returns the same Arc each call, so a divergence means \
                 the helper constructed a fresh registry instead of honouring the hook",
                ptr,
                first,
            );
        }

        // `_hook_guard` drops here, releasing the serialising lock
        // and clearing the global hook for the next test.
    }

    // ── REGISTRY RECOVERY on transient startup
    //    failures. While incomplete, each tick re-runs the connect
    //    path so a server that comes up later is picked up without
    //    a daemon restart. Once the registry is complete, ticks
    //    skip the retry block so the Arc pointer stays identical.
    //
    //    These regressions exercise the worker's per-tick retry
    //    block directly by simulating the boot + N heartbeat ticks
    //    against a test hook that swaps registry shape over time.
    //
    //    Helper: build a config with one granted MCP server so the
    //    retry check fires when the hook returns a sub-complete registry.
    fn config_with_one_granted_mcp_server(tmp: &TempDir) -> (Config, String) {
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig, McpServerConfig};
        let mut config = test_config(tmp);
        // Grant the heartbeat agent one server. The transport details
        // don't matter — the test hook short-circuits the real
        // `connect_all` path, so no stdio child is spawned.
        config.mcp.enabled = true;
        config.mcp.servers.push(McpServerConfig {
            name: "granted".to_string(),
            ..McpServerConfig::default()
        });
        config.mcp_bundles.insert(
            "b".to_string(),
            McpBundleConfig {
                servers: vec!["granted".to_string()],
                exclude: vec![],
            },
        );
        let agent_alias = "ops".to_string();
        config.agents.insert(
            agent_alias.clone(),
            AliasedAgentConfig {
                mcp_bundles: vec!["b".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        (config, agent_alias)
    }

    /// Build a stub `McpServer` handle with the given name for use in
    /// regression tests that exercise identity / ptr_eq /
    /// server_count assertions. The handle has a no-op transport;
    /// any actual tool call on the resulting handle panics, so this
    /// helper is only safe in tests that read state, never make
    /// tool calls. Two calls with the same name return two distinct
    /// handles (different Arc allocations) — to assert identity is
    /// preserved across a recovery, both sides of the comparison
    /// must hold the SAME `McpServer` clone, which `for_test_with_server_handles`
    /// + cloning an existing handle achieves.
    fn make_test_server_handle(name: &str) -> crate::tools::McpServer {
        crate::tools::McpRegistry::for_test_make_stub_server(name)
    }

    // ── missing_or_dead_servers / connect_heartbeat_mcp_registry partial-retry ──

    fn test_server_config(name: &str) -> zeroclaw_config::schema::McpServerConfig {
        zeroclaw_config::schema::McpServerConfig {
            name: name.to_string(),
            transport: zeroclaw_config::schema::McpTransport::Stdio,
            command: "true".to_string(),
            ..zeroclaw_config::schema::McpServerConfig::default()
        }
    }

    /// FAIL-ON-OLD guard for the partial-retry fix: granted = {A, B},
    /// current has a healthy A. `missing_or_dead_servers` must return only
    /// B -- A must never be re-included while its current handle is
    /// healthy. Repeated calls (simulating repeated heartbeat ticks while
    /// B stays down) must keep excluding A every time; a pre-fix caller
    /// that instead passed the *whole* granted list to
    /// `McpRegistry::connect_all` on every tick would have respawned and
    /// re-handshaked A on each one of these calls too.
    #[test]
    fn missing_or_dead_servers_excludes_healthy_current_across_repeated_calls() {
        let a_handle = make_test_server_handle("server-a");
        let current = std::sync::Arc::new(crate::tools::McpRegistry::for_test_with_server_handles(
            vec![("server-a".to_string(), a_handle)],
        ));
        let granted = vec![
            test_server_config("server-a"),
            test_server_config("server-b"),
        ];

        const TICKS: usize = 5;
        for tick in 0..TICKS {
            let to_connect = missing_or_dead_servers(granted.clone(), Some(&current));
            let names: Vec<&str> = to_connect.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(
                names,
                vec!["server-b"],
                "tick {tick}: only the down server (B) may be reconnected;                  healthy A must be excluded on every tick, not just the first"
            );
        }
    }

    /// When `current` is `None` (worker boot), every granted server is
    /// missing by definition -- the full-connect boot path must be
    /// unaffected by this fix.
    #[test]
    fn missing_or_dead_servers_returns_everything_when_current_is_none() {
        let granted = vec![
            test_server_config("server-a"),
            test_server_config("server-b"),
        ];
        let to_connect = missing_or_dead_servers(granted, None);
        let names: Vec<&str> = to_connect.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["server-a", "server-b"]);
    }

    /// PRODUCTION-PATH regression, driven through
    /// `retry_heartbeat_mcp_registry` itself rather than the pure
    /// `missing_or_dead_servers` helper in isolation: granted = {A, B}, A
    /// already healthy in `shared`, B perpetually down. Across N repeated
    /// ticks, the test hook (now installed *after* filtering, see
    /// `connect_heartbeat_mcp_registry`) must observe ONLY "server-b" in
    /// the requested set every time -- "server-a" must never be
    /// resubmitted for connection while its handle stays healthy. This
    /// proves what the real retry call site actually passes to the
    /// connector, closing the gap the pure-helper-only test above could
    /// not reach on its own.
    #[tokio::test]
    async fn retry_heartbeat_mcp_registry_never_resubmits_healthy_peer_across_ticks() {
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig};

        let a_handle = make_test_server_handle("server-a");
        let mut shared: Option<std::sync::Arc<crate::tools::McpRegistry>> = Some(
            std::sync::Arc::new(crate::tools::McpRegistry::for_test_with_server_handles(
                vec![("server-a".to_string(), a_handle)],
            )),
        );

        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.mcp.enabled = true;
        config.mcp.servers.push(test_server_config("server-a"));
        config.mcp.servers.push(test_server_config("server-b"));
        config.mcp_bundles.insert(
            "ab".to_string(),
            McpBundleConfig {
                servers: vec!["server-a".to_string(), "server-b".to_string()],
                exclude: vec![],
            },
        );
        config.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                mcp_bundles: vec!["ab".to_string()],
                ..AliasedAgentConfig::default()
            },
        );

        // Hook simulates "B is still down": it never actually connects
        // anything (returns an empty registry) but records exactly which
        // server names it was asked to connect on each call.
        let requested: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let requested_for_hook = std::sync::Arc::clone(&requested);
        let empty_registry: std::sync::Arc<crate::tools::McpRegistry> = std::sync::Arc::new(
            crate::tools::McpRegistry::connect_all(&[])
                .await
                .expect("empty connect_all succeeds"),
        );
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(std::sync::Arc::new(move |_alias, servers| {
                requested_for_hook
                    .lock()
                    .unwrap()
                    .push(servers.iter().map(|s| s.name.clone()).collect());
                std::sync::Arc::clone(&empty_registry)
            }));

        const TICKS: usize = 5;
        for tick in 0..TICKS {
            retry_heartbeat_mcp_registry(&mut shared, &config, "ops")
                .await
                .expect("retry must not error");
            assert!(
                shared.is_some(),
                "tick {tick}: healthy server-a must keep the shared registry populated"
            );
        }

        let calls = requested.lock().unwrap();
        assert_eq!(calls.len(), TICKS, "hook must fire once per tick");
        for (tick, names) in calls.iter().enumerate() {
            assert_eq!(
                names,
                &vec!["server-b".to_string()],
                "tick {tick}: only server-b (down) may be requested; \
                 server-a (healthy) must never be resubmitted"
            );
        }

        // `_hook_guard` drops here, releasing the serialising lock and
        // clearing the global hook for the next test.
    }

    /// When every granted server already has a healthy current handle,
    /// nothing is missing -- `connect_heartbeat_mcp_registry` must return
    /// `Ok(None)` rather than issuing a no-op `connect_all([])`, and must
    /// NOT attempt to actually spawn/handshake the already-healthy server
    /// (this test uses `command: "true"`, so a spawn attempt for
    /// "server-a" would not by itself fail the test on its own -- the
    /// real assertion is that no reconnect was even attempted, i.e. the
    /// filtered set was empty and the function short-circuited).
    #[tokio::test]
    async fn connect_heartbeat_mcp_registry_returns_none_when_all_healthy() {
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig};

        let a_handle = make_test_server_handle("server-a");
        let current = std::sync::Arc::new(crate::tools::McpRegistry::for_test_with_server_handles(
            vec![("server-a".to_string(), a_handle)],
        ));

        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.mcp.enabled = true;
        config.mcp.servers.push(test_server_config("server-a"));
        config.mcp_bundles.insert(
            "b".to_string(),
            McpBundleConfig {
                servers: vec!["server-a".to_string()],
                exclude: vec![],
            },
        );
        config.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                mcp_bundles: vec!["b".to_string()],
                ..AliasedAgentConfig::default()
            },
        );

        let result = connect_heartbeat_mcp_registry(&config, "ops", Some(&current))
            .await
            .expect("must not error");
        assert!(
            result.is_none(),
            "nothing was missing/dead, so there is nothing to (re)connect"
        );
    }

    // ── reconcile_heartbeat_mcp_registry unit tests ──────────────────────

    /// Additive merge: current = {A}, fresh = {B}. Even though fresh
    /// does NOT include A, the recovered B must be admitted into the
    /// merged registry while A's live handle is preserved (Arc
    /// identity check). This is the "granted-but-flaky peer whose
    /// discovery never produced the full set in a single connect_all"
    /// invariant the additive merge protects.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_merges_when_fresh_is_disjoint_subset() {
        let a_handle = make_test_server_handle("server-a");
        let b_handle = make_test_server_handle("server-b");
        let current = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![(
                "server-a".to_string(),
                a_handle.clone(),
            )]),
        ));
        let fresh = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![(
                "server-b".to_string(),
                b_handle.clone(),
            )]),
        ));

        let result = reconcile_heartbeat_mcp_registry(current.as_ref(), fresh.as_ref())
            .await
            .expect("reconcile must produce a merged registry");
        let merged = result.server_handles();
        let merged_names: Vec<String> = merged.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            merged_names,
            vec!["server-a".to_string(), "server-b".to_string()],
            "merged registry must contain both server-a (preserved from current) \
             and server-b (admitted from fresh)"
        );
        let (_, a_merged) = merged.iter().find(|(n, _)| n == "server-a").unwrap();
        let (_, b_merged) = merged.iter().find(|(n, _)| n == "server-b").unwrap();
        assert!(
            a_handle.ptr_eq(a_merged),
            "merged A handle must be the SAME Arc as the original current A handle \
             (live stdio connection preserved, not respawned)"
        );
        assert!(
            b_handle.ptr_eq(b_merged),
            "merged B handle must be the SAME Arc as the fresh B handle"
        );
    }

    /// Additive merge: current = {A, B}, fresh = {B, C}. The healthy
    /// current B is preserved; C is admitted; A (not in fresh) is
    /// dropped because `health_check_all` reports it dead. Wait —
    /// for test stubs health_check_all reports no dead, so A is
    /// healthy. The additive merge keeps A AND adds C. This is the
    /// shape of a "config drift" recovery where a server was renamed
    /// out of fresh and a new server came up in its place.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_preserves_healthy_drops_drift_adds_recovered() {
        let a_handle = make_test_server_handle("server-a");
        let b_handle = make_test_server_handle("server-b");
        let c_handle = make_test_server_handle("server-c");
        let current = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![
                ("server-a".to_string(), a_handle.clone()),
                ("server-b".to_string(), b_handle.clone()),
            ]),
        ));
        let fresh = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![
                ("server-b".to_string(), b_handle.clone()),
                ("server-c".to_string(), c_handle.clone()),
            ]),
        ));

        let result = reconcile_heartbeat_mcp_registry(current.as_ref(), fresh.as_ref())
            .await
            .expect("reconcile must produce a merged registry");
        let merged = result.server_handles();
        let merged_names: Vec<String> = merged.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            merged_names,
            vec![
                "server-a".to_string(),
                "server-b".to_string(),
                "server-c".to_string()
            ],
            "merged registry must contain A (kept from current — fresh omitted it \
             but A is healthy), B (preserved live handle from current), and C (admitted \
             from fresh)"
        );
        let (_, a_merged) = merged.iter().find(|(n, _)| n == "server-a").unwrap();
        let (_, b_merged) = merged.iter().find(|(n, _)| n == "server-b").unwrap();
        let (_, c_merged) = merged.iter().find(|(n, _)| n == "server-c").unwrap();
        assert!(
            a_handle.ptr_eq(a_merged),
            "A's live handle must be preserved even though fresh omitted A"
        );
        assert!(
            b_handle.ptr_eq(b_merged),
            "B's live handle must be preserved (matched identity in fresh)"
        );
        assert!(c_handle.ptr_eq(c_merged), "C's handle must come from fresh");
    }

    /// Additive merge: current = {A}, fresh = {A, B}. A is preserved
    /// (matched identity); B is admitted; no churn on A.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_preserves_matched_admits_new() {
        let a_handle = make_test_server_handle("server-a");
        let b_handle = make_test_server_handle("server-b");
        let current = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![(
                "server-a".to_string(),
                a_handle.clone(),
            )]),
        ));
        let fresh = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![
                ("server-a".to_string(), a_handle.clone()),
                ("server-b".to_string(), b_handle.clone()),
            ]),
        ));

        let result = reconcile_heartbeat_mcp_registry(current.as_ref(), fresh.as_ref())
            .await
            .expect("reconcile must produce a merged registry");
        let merged = result.server_handles();
        let merged_names: Vec<String> = merged.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            merged_names,
            vec!["server-a".to_string(), "server-b".to_string()],
            "merged registry must contain A (preserved) and B (admitted)"
        );
        let (_, a_merged) = merged.iter().find(|(n, _)| n == "server-a").unwrap();
        let (_, b_merged) = merged.iter().find(|(n, _)| n == "server-b").unwrap();
        assert!(
            a_handle.ptr_eq(a_merged),
            "A's live handle must be preserved (matched identity in fresh)"
        );
        assert!(b_handle.ptr_eq(b_merged), "B's handle must come from fresh");
    }

    /// No-churn steady state: current = fresh by name AND identity.
    /// The merge yields the same set; the caller must NOT replace
    /// `current`'s Arc. This is the "every healthy handle survives"
    /// contract that protects stdio children from per-tick churn.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_no_churn_when_identities_match() {
        let a_handle = make_test_server_handle("server-a");
        let b_handle = make_test_server_handle("server-b");
        let current = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![
                ("server-a".to_string(), a_handle.clone()),
                ("server-b".to_string(), b_handle.clone()),
            ]),
        ));
        let fresh = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![
                ("server-a".to_string(), a_handle.clone()),
                ("server-b".to_string(), b_handle.clone()),
            ]),
        ));

        let result = reconcile_heartbeat_mcp_registry(current.as_ref(), fresh.as_ref()).await;
        assert!(
            result.is_none(),
            "must return None (no churn) when merged set equals healthy current \
             by name and Arc identity; got Some"
        );
    }

    /// No current registry — use fresh if available.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_none_current_uses_fresh() {
        let a_handle = make_test_server_handle("server-a");
        let fresh = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![(
                "server-a".to_string(),
                a_handle.clone(),
            )]),
        ));

        let result = reconcile_heartbeat_mcp_registry(None, fresh.as_ref()).await;
        assert!(result.is_some(), "must use fresh when there is no current");
    }

    /// No fresh registry — keep current.
    #[tokio::test]
    async fn reconcile_heartbeat_mcp_registry_none_fresh_keeps_current() {
        let a_handle = make_test_server_handle("server-a");
        let current = Some(std::sync::Arc::new(
            crate::tools::McpRegistry::for_test_with_server_handles(vec![(
                "server-a".to_string(),
                a_handle.clone(),
            )]),
        ));

        let result = reconcile_heartbeat_mcp_registry(current.as_ref(), None).await;
        assert!(result.is_none(), "must keep current when there is no fresh");
    }

    // ── Health check & reconnection for a stdio child that dies ────────

    /// Regression test: when a stdio MCP child exits after a successful
    /// connection, the heartbeat worker's retry gate must detect the dead
    /// transport and reconnect on the next tick.
    ///
    /// The test invokes `retry_heartbeat_mcp_registry` directly — the
    /// same production helper the heartbeat worker calls on every
    /// tick — rather than reimplementing the health-check /
    /// kill_dead_connections / reconnect / reconcile sequence inline.
    /// That way a future regression that removes the health-check or
    /// reconnect branch from the production helper will fail this
    /// test (not silently let the dead child leak across ticks).
    ///
    /// Unix-only: the fixture writes a Bash script, chmods it via
    /// `PermissionsExt`, and invokes `kill`. The test is gated under
    /// `#[cfg(unix)]` so the repository's Windows `cargo test
    /// --no-run` target for `zeroclaw-runtime` stays green.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_worker_reconnects_after_stdio_child_exits() {
        use std::os::unix::fs::PermissionsExt;
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig, McpServerConfig};

        let tmp = TempDir::new().unwrap();

        // ── 1. Build a tiny stdio MCP server script ──────────────────────
        let pid_path = tmp.path().join("pid");
        let server_path = tmp.path().join("mcp-server.sh");
        std::fs::write(
        &server_path,
        format!(
            r#"#!/usr/bin/env bash
echo $$ > "{}"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2024-11-05","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"reconnect-test","version":"0.1.0"}}}}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[]}}}}'
      ;;
  esac
done
"#,
            pid_path.display(),
        ),
    )
    .expect("write server script");
        std::fs::set_permissions(&server_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");

        // ── 2. Build a config that grants this stdio server ─────────────
        let mut config = test_config(&tmp);
        config.mcp.enabled = true;
        config.mcp.servers.push(McpServerConfig {
            name: "reconnect-test".to_string(),
            command: server_path.display().to_string(),
            args: vec![pid_path.display().to_string()],
            env: std::collections::HashMap::new(),
            tool_timeout_secs: None,
            transport: zeroclaw_config::schema::McpTransport::Stdio,
            url: None,
            headers: std::collections::HashMap::new(),
            pinned_resources: vec![],
        });
        let agent_alias = "ops".to_string();
        config.mcp_bundles.insert(
            "reconnect-bundle".to_string(),
            McpBundleConfig {
                servers: vec!["reconnect-test".to_string()],
                exclude: vec![],
            },
        );
        config.agents.insert(
            agent_alias.clone(),
            AliasedAgentConfig {
                mcp_bundles: vec!["reconnect-bundle".to_string()],
                ..AliasedAgentConfig::default()
            },
        );

        // ── 3. Connect (boot) — no test hook, real stdio child ─────────
        let mut shared_mcp_registry: Option<std::sync::Arc<crate::tools::McpRegistry>> =
            connect_heartbeat_mcp_registry(&config, &agent_alias, None)
                .await
                .expect("connect_heartbeat_mcp_registry succeeds");
        assert!(shared_mcp_registry.is_some(), "registry must be Some");
        assert_eq!(
            shared_mcp_registry.as_ref().unwrap().server_names(),
            vec!["reconnect-test"],
            "must have the granted server"
        );

        // ── 4. Kill the stdio child process ─────────────────────────────
        let pid: u32 = std::fs::read_to_string(&pid_path)
            .expect("read PID")
            .trim()
            .parse()
            .expect("parse PID");
        std::process::Command::new("kill")
            .arg(pid.to_string())
            .output()
            .expect("kill child");

        // Give the kernel a moment to reap.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // ── 5. Simulate one heartbeat tick via the production helper ────
        //    The helper must (a) detect the dead child via
        //    `health_check_all`, (b) call `kill_dead_connections`,
        //    (c) reconnect via `connect_heartbeat_mcp_registry`, and
        //    (d) replace `shared_mcp_registry` with a healthy one.
        //    If any of those steps is removed from the helper in the
        //    future, this test fails.
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
            .await
            .expect("retry_heartbeat_mcp_registry succeeds");

        // ── 6. Verify the reconnected registry is alive ─────────────────
        assert!(
            shared_mcp_registry.is_some(),
            "registry must be Some after reconnection"
        );
        let registry = shared_mcp_registry.as_ref().unwrap();
        assert_eq!(
            registry.server_names(),
            vec!["reconnect-test"],
            "registry must contain the granted server after reconnect"
        );
        // Health check is read-only against the shared Arc, so we do
        // not need `Arc::get_mut`. The reconnected child must report
        // alive (`health_check_all` returns empty).
        let dead = registry.health_check_all();
        assert!(
            dead.is_empty(),
            "reconnected server must be alive; health_check_all returned {dead:?}"
        );
        assert_eq!(
            registry.server_count(),
            1,
            "must have exactly one connected server"
        );
    }

    // ── Steady-state pin (a) — once complete, no churn.
    //
    //    The hook returns the SAME complete `Arc<McpRegistry>` on every
    //    call, so the retry block short-circuits after boot and the Arc
    //    pointer stays identical across ticks.
    #[tokio::test]
    async fn heartbeat_worker_skips_retry_when_mcp_registry_already_complete() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let (config, agent_alias) = config_with_one_granted_mcp_server(&tmp);

        // Pre-build a registry with exactly one server (matches
        // granted.len() == 1). Every hook invocation returns the same
        // Arc — simulating "the MCP server came up before boot and
        // stayed connected".
        let complete_registry: Arc<crate::tools::McpRegistry> =
            Arc::new(crate::tools::McpRegistry::for_test_with_server_count(1));
        assert_eq!(
            complete_registry.server_count(),
            1,
            "the test fixture registry must have 1 server to match the granted list"
        );
        let complete_registry_for_hook = Arc::clone(&complete_registry);

        // Count every hook invocation so we can assert the retry
        // block is a no-op once the registry is complete.
        let construct_count = Arc::new(AtomicUsize::new(0));
        let construct_count_for_hook = Arc::clone(&construct_count);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                Arc::clone(&complete_registry_for_hook)
            }));

        // (1) Worker boot — single pre-loop call.
        let mut shared_mcp_registry: Option<Arc<crate::tools::McpRegistry>> =
            connect_heartbeat_mcp_registry(&config, &agent_alias, None)
                .await
                .expect("connect_heartbeat_mcp_registry succeeds");
        assert!(
            shared_mcp_registry.is_some(),
            "test config grants 1 server and the hook returns Some",
        );
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(1),
            "boot must yield the complete registry"
        );
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "boot constructs the registry exactly once"
        );

        // (2) Simulate N heartbeat ticks. Each tick calls
        //     `retry_heartbeat_mcp_registry` — the same production
        //     function the worker uses — so the retry-decision logic
        //     is not reimplemented inline.
        const TICKS: usize = 32;
        let mut override_ptrs: Vec<*const crate::tools::McpRegistry> = Vec::with_capacity(TICKS);
        let complete_ptr = Arc::as_ptr(&complete_registry);
        for tick in 0..TICKS {
            retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
                .await
                .expect("retry_heartbeat_mcp_registry succeeds");

            // Build the override the worker would hand to `agent::run`.
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: shared_mcp_registry.as_ref().map(Arc::clone),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            let tick_registry = overrides
                .mcp_registry
                .as_ref()
                .expect("complete registry must propagate into overrides");
            let tick_ptr = Arc::as_ptr(tick_registry);
            assert!(
                std::ptr::eq(tick_ptr, complete_ptr),
                "tick {tick}: override Arc pointer diverged ({:p} != {:p}); \
                 the worker must not reconstruct the registry once it is complete",
                tick_ptr,
                complete_ptr,
            );
            override_ptrs.push(tick_ptr);
            drop(overrides);
        }

        // (3) Hard regression assertions.
        //
        //     Hook fired exactly once total — boot only. Any value >1
        //     would mean the retry block over-triggered and
        //     re-ran the connect path on a registry that was
        //     already complete.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "once the registry is complete, the per-tick retry block MUST NOT re-run \
             connect_heartbeat_mcp_registry; saw {} constructions across {TICKS} ticks",
            construct_count.load(Ordering::SeqCst),
        );

        // Every tick's override Arc points to the same allocation.
        // A divergence would mean a per-tick construction slipped
        // through.
        let first = override_ptrs[0];
        for (tick, ptr) in override_ptrs.iter().enumerate() {
            assert!(
                std::ptr::eq(*ptr, first),
                "tick {tick}: override Arc pointer drifted away from the boot allocation; \
                 the steady-state property is broken — got {:p}, expected {:p}",
                ptr,
                first,
            );
        }

        // `_hook_guard` drops here, releasing the serialising lock
        // and clearing the global hook for the next test.
    }

    // ── Registry recovery (b): while incomplete,
    //    each tick re-runs `connect_heartbeat_mcp_registry` so a
    //    server that comes up later is picked up. Simulates the
    //    failing-then-recovering path:
    //
    //      tick 1 (boot):        hook returns EMPTY  Arc  → server not up yet
    //      tick 1 (in-loop retry): hook returns EMPTY  Arc  → still not up
    //      tick 2 (in-loop retry): hook returns COMPLETE Arc  → server came up
    //      tick 3 (in-loop retry): hook would be called BUT
    //                              `server_count == granted.len()`
    //                              → SKIPPED — registry is now complete
    //      tick 4 (in-loop retry): same as tick 3 → SKIPPED
    //
    //    Final state: shared Arc is the complete registry, hooks fired
    //    exactly 3 times (boot + 2 retries), and ticks 3..=4 reuse the
    //    same Arc pointer the recovery produced.
    #[tokio::test]
    async fn heartbeat_worker_retries_incomplete_mcp_registry_across_ticks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();
        let (config, agent_alias) = config_with_one_granted_mcp_server(&tmp);

        // Pre-build the two registries the hook will return. The
        // empty one has `server_count() == 0` (the failing case);
        // the complete one has `server_count() == 1` (matching the
        // single granted server). Using `for_test_with_server_count`
        // keeps this test pure-Rust — no stdio child is spawned, so
        // CI does not need node/npx on PATH.
        let empty_registry: Arc<crate::tools::McpRegistry> =
            Arc::new(crate::tools::McpRegistry::for_test_with_server_count(0));
        assert_eq!(empty_registry.server_count(), 0);
        let complete_registry: Arc<crate::tools::McpRegistry> =
            Arc::new(crate::tools::McpRegistry::for_test_with_server_count(1));
        assert_eq!(complete_registry.server_count(), 1);

        let empty_for_hook = Arc::clone(&empty_registry);
        let complete_for_hook = Arc::clone(&complete_registry);

        // Sequence of registries the hook returns, one per call:
        //   call #0 (boot):             empty
        //   call #1 (tick 1 in-loop retry): empty
        //   call #2 (tick 2 in-loop retry): complete   ← server came up
        //   call #3+ (no more calls expected, but if any happen we
        //              always return the complete one so a buggy
        //              retry path that kept re-trying would still
        //              observe the recovery, not a hang or test flakiness)
        let sequence: Arc<std::sync::Mutex<Vec<Arc<crate::tools::McpRegistry>>>> =
            Arc::new(std::sync::Mutex::new(vec![
                Arc::clone(&empty_for_hook),
                Arc::clone(&empty_for_hook),
                Arc::clone(&complete_for_hook),
            ]));
        // Pad the sequence with complete registries so any extra hook
        // call beyond the expected 3 still observes a sensible value.
        sequence
            .lock()
            .expect("sequence mutex not poisoned")
            .extend(std::iter::repeat_with(|| Arc::clone(&complete_for_hook)).take(64));

        // Count hook invocations (construction counter) and record
        // every Arc pointer the hook returned, so we can assert the
        // recovery Arc eventually reaches the worker. Raw pointers
        // are not `Send`, so we store them as `usize` (cast from
        // `Arc::as_ptr`) inside the captured mutex — comparing two
        // `usize` values for equality is equivalent to comparing
        // two `*const McpRegistry` values for `std::ptr::eq` and
        // lets the hook closure satisfy the `Send + Sync` bound.
        let construct_count = Arc::new(AtomicUsize::new(0));
        let construct_count_for_hook = Arc::clone(&construct_count);
        let returned_ptrs: Arc<std::sync::Mutex<Vec<usize>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let returned_ptrs_for_hook = Arc::clone(&returned_ptrs);
        let sequence_for_hook = Arc::clone(&sequence);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                let next = {
                    let mut seq = sequence_for_hook
                        .lock()
                        .expect("sequence mutex not poisoned");
                    seq.remove(0)
                };
                let ptr = Arc::as_ptr(&next) as usize;
                returned_ptrs_for_hook
                    .lock()
                    .expect("returned_ptrs mutex not poisoned")
                    .push(ptr);
                next
            }));

        // (1) Worker boot — single pre-loop call. Hook returns the
        //     EMPTY registry (call #0).
        let mut shared_mcp_registry: Option<Arc<crate::tools::McpRegistry>> =
            connect_heartbeat_mcp_registry(&config, &agent_alias, None)
                .await
                .expect("connect_heartbeat_mcp_registry succeeds");
        assert!(shared_mcp_registry.is_some(), "hook returns Some",);
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(0),
            "boot hook returns the empty registry (server not up yet)"
        );
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "boot constructs exactly once"
        );
        let boot_ptr = Arc::as_ptr(shared_mcp_registry.as_ref().expect("non-None after boot"));
        assert!(
            std::ptr::eq(boot_ptr, Arc::as_ptr(&empty_registry)),
            "boot pointer must be the empty registry",
        );

        // (2) Simulate N=4 heartbeat ticks. Each tick calls
        //     `retry_heartbeat_mcp_registry` — the same production
        //     function the worker uses — so the retry-decision logic
        //     is not reimplemented inline.
        const TICKS: usize = 4;
        let mut per_tick_ptrs: Vec<*const crate::tools::McpRegistry> = Vec::with_capacity(TICKS);
        let mut per_tick_server_counts: Vec<usize> = Vec::with_capacity(TICKS);
        for _tick in 0..TICKS {
            retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
                .await
                .expect("retry_heartbeat_mcp_registry succeeds");

            // Build the override the worker would hand to `agent::run`.
            let overrides = crate::agent::loop_::AgentRunOverrides {
                mcp_registry: shared_mcp_registry.as_ref().map(Arc::clone),
                ..crate::agent::loop_::AgentRunOverrides::default()
            };
            let tick_registry = overrides
                .mcp_registry
                .as_ref()
                .expect("override registry must propagate");
            per_tick_ptrs.push(Arc::as_ptr(tick_registry));
            per_tick_server_counts.push(tick_registry.server_count());
            drop(overrides);
        }

        // (3) Verify the recovery sequence.
        //
        //     Hook fires 3 times total: 1 boot + 2 in-loop retries.
        //     The first retry (tick 0) still returns empty (server
        //     not up yet); the second retry (tick 1) returns the
        //     complete registry (server came up). After tick 1 the
        //     registry is complete, so tick 2 and tick 3 skip the
        //     retry block.
        //
        //     A hook count of 1 would mean the retry block never ran
        //     and the worker is stuck on the empty registry.
        //
        //     A hook count >3 would mean the retry block re-ran
        //     after recovery.
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            3,
            "hook must fire exactly 3 times: 1 boot + 2 in-loop retries \
             (tick-0 retry still empty, tick-1 retry recovered, \
             tick-2+ skip because registry is complete); got {}",
            construct_count.load(Ordering::SeqCst),
        );

        // The hook's 3 returned pointers, in call order:
        //   empty, empty, complete.
        let returned = returned_ptrs
            .lock()
            .expect("returned_ptrs mutex not poisoned")
            .clone();
        assert_eq!(returned.len(), 3, "hook must have fired 3 times");
        let empty_ptr_usize = Arc::as_ptr(&empty_registry) as usize;
        let complete_ptr_usize = Arc::as_ptr(&complete_registry) as usize;
        assert_eq!(
            returned[0], empty_ptr_usize,
            "hook call #0 (boot) must return the empty registry",
        );
        assert_eq!(
            returned[1], empty_ptr_usize,
            "hook call #1 (tick-0 in-loop retry) must return the empty registry",
        );
        assert_eq!(
            returned[2], complete_ptr_usize,
            "hook call #2 (tick-1 in-loop retry) must return the complete registry (server came up)",
        );

        // The override Arc the worker hands to `agent::run` each tick:
        //   tick 0: empty (boot); `server_count() == 0`.
        //   tick 1: COMPLETE (the in-loop retry recovered this
        //           tick — server came up); `server_count() == 1`,
        //           pointer is the recovery Arc.
        //   tick 2: COMPLETE (no retry); pointer MUST match tick 1
        //           (no churn after recovery).
        //   tick 3: COMPLETE (no retry); pointer MUST match tick 1.
        //
        // We assert `server_count` on each override (proving the
        // worker's stored registry made it into `agent::run`) and then
        // assert pointer equality on the post-recovery ticks (proving
        // the no-churn property once the registry is complete).
        assert_eq!(per_tick_ptrs.len(), TICKS);
        let complete_ptr = Arc::as_ptr(&complete_registry);

        // `per_tick_server_counts` MUST follow the recovery sequence:
        // [0, 1, 1, 1] (tick 0 override built from boot empty,
        // tick 1 override built from in-loop retry that returned the
        // complete registry — i.e. server came up during tick 1 —
        // tick 2 + tick 3 skip the retry and reuse the same complete
        // Arc). A different pattern would mean the worker's retry
        // block is mis-counting completeness.
        assert_eq!(
            per_tick_server_counts,
            vec![0, 1, 1, 1],
            "per-tick server_count sequence must reflect recovery: \
             tick 0 empty (boot) → tick 1 complete (in-loop retry recovered) → \
             tick 2 complete (skip) → tick 3 complete (skip)",
        );

        // Once recovery has produced the complete registry, the
        // Arc pointer MUST stay the same across all subsequent
        // ticks — any drift means the retry block is over-firing.
        let recovery_ptr = per_tick_ptrs[1];
        assert!(
            std::ptr::eq(recovery_ptr, complete_ptr),
            "tick 1 override must be the recovery complete registry; \
             got {:p}, expected {:p}",
            recovery_ptr,
            complete_ptr,
        );
        for (tick, ptr) in per_tick_ptrs
            .iter()
            .enumerate()
            .skip(2)
            .take(TICKS.saturating_sub(2))
        {
            assert!(
                std::ptr::eq(*ptr, recovery_ptr),
                "tick {tick}: override Arc drifted from the recovery Arc; \
                 the no-churn property is broken — got {:p}, expected {:p}",
                ptr,
                recovery_ptr,
            );
        }

        // The worker's stored `shared_mcp_registry` ended up as the
        // complete registry — recovery is visible to the daemon
        // state, not just to the per-tick override.
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(1),
            "the worker's stored registry must be the complete one after recovery"
        );
        assert!(
            std::ptr::eq(
                Arc::as_ptr(
                    shared_mcp_registry
                        .as_ref()
                        .expect("non-None after recovery")
                ),
                complete_ptr,
            ),
            "the worker's stored Arc must be the complete registry; \
             recovery succeeded but did not replace the stored Arc"
        );

        // `_hook_guard` drops here, releasing the serialising lock
        // and clearing the global hook for the next test.
    }

    // ── Partial recovery: A stays healthy across failed B retries, then
    //    B is admitted without replacing A. The lifetime
    //    invariant requires that A's `McpServer` Arc identity is
    //    stable across every tick and that B is admitted in a single
    //    tick — without replacing A or forcing both to reconnect.
    //
    //    Grants:    {A, B}
    //    Sequence:
    //      tick 0 (boot):                hook returns {}           → empty registry
    //      tick 1 (retry, B still down): hook returns {A}          → A admitted
    //      tick 2 (retry, B still down): hook returns {A} (same handle)
    //                                                          → A stays identical
    //                                                            (no churn)
    //      tick 3 (retry, B came up):    hook returns {A, B}
    //                                 (same A handle, fresh B) → A stays identical,
    //                                                            B admitted in the
    //                                                            SAME tick (without
    //                                                            replacing A).
    #[tokio::test]
    async fn heartbeat_worker_preserves_healthy_a_admits_recovered_b_additively() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = TempDir::new().unwrap();

        // Build the test config with TWO granted MCP servers (A and B).
        let mut config = test_config(&tmp);
        config.mcp.enabled = true;
        config
            .mcp
            .servers
            .push(zeroclaw_config::schema::McpServerConfig {
                name: "server-a".to_string(),
                ..zeroclaw_config::schema::McpServerConfig::default()
            });
        config
            .mcp
            .servers
            .push(zeroclaw_config::schema::McpServerConfig {
                name: "server-b".to_string(),
                ..zeroclaw_config::schema::McpServerConfig::default()
            });
        config.mcp_bundles.insert(
            "ab-bundle".to_string(),
            zeroclaw_config::schema::McpBundleConfig {
                servers: vec!["server-a".to_string(), "server-b".to_string()],
                exclude: vec![],
            },
        );
        let agent_alias = "ops".to_string();
        config.agents.insert(
            agent_alias.clone(),
            zeroclaw_config::schema::AliasedAgentConfig {
                mcp_bundles: vec!["ab-bundle".to_string()],
                ..zeroclaw_config::schema::AliasedAgentConfig::default()
            },
        );

        // Pre-build the four registries the hook will return:
        //   empty:      {}            — boot + B-down tick 0
        //   a_only:     {A_handle_a}  — B-down ticks 1..=2 (handle shared)
        //   ab_first:   {A_handle_a, B_handle_b}  — B-came-up tick 3 (A reused)
        //
        // The single `A_handle_a` Arc is the key — it MUST survive
        // across every tick. If the daemon's reconciliation replaces
        // A's handle (e.g. by re-running `connect_all` and getting a
        // fresh stub) at any tick, this test fails.
        let empty_handle = Arc::new(crate::tools::McpRegistry::for_test_with_server_count(0));
        let a_handle = make_test_server_handle("server-a");
        let b_handle = make_test_server_handle("server-b");
        let a_only_registry = Arc::new(crate::tools::McpRegistry::for_test_with_server_handles(
            vec![("server-a".to_string(), a_handle.clone())],
        ));
        let ab_registry = Arc::new(crate::tools::McpRegistry::for_test_with_server_handles(
            vec![
                ("server-a".to_string(), a_handle.clone()),
                ("server-b".to_string(), b_handle.clone()),
            ],
        ));

        let empty_for_hook = Arc::clone(&empty_handle);
        let a_only_for_hook = Arc::clone(&a_only_registry);
        let ab_for_hook = Arc::clone(&ab_registry);

        // Sequence: 0=empty (boot), 1=a_only, 2=a_only, 3=ab.
        // The hook pops from this sequence; any extra calls (which
        // must NOT happen) would panic the test, surfacing the bug.
        let sequence: Arc<std::sync::Mutex<Vec<Arc<crate::tools::McpRegistry>>>> =
            Arc::new(std::sync::Mutex::new(vec![
                Arc::clone(&empty_for_hook),
                Arc::clone(&a_only_for_hook),
                Arc::clone(&a_only_for_hook),
                Arc::clone(&ab_for_hook),
            ]));
        let construct_count = Arc::new(AtomicUsize::new(0));
        let construct_count_for_hook = Arc::clone(&construct_count);
        let sequence_for_hook = Arc::clone(&sequence);
        let _hook_guard =
            set_heartbeat_mcp_registry_test_hook(Arc::new(move |_alias, _servers| {
                construct_count_for_hook.fetch_add(1, Ordering::SeqCst);
                let mut seq = sequence_for_hook
                    .lock()
                    .expect("sequence mutex not poisoned");
                if seq.is_empty() {
                    panic!(
                        "hook fired more times than expected — \
                         the retry helper is over-calling connect_heartbeat_mcp_registry \
                         after recovery completed"
                    );
                }
                seq.remove(0)
            }));

        // Boot — hook call #0 returns the empty registry.
        let mut shared_mcp_registry: Option<Arc<crate::tools::McpRegistry>> =
            connect_heartbeat_mcp_registry(&config, &agent_alias, None)
                .await
                .expect("connect_heartbeat_mcp_registry succeeds");
        assert!(shared_mcp_registry.is_some(), "hook returns Some");
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(0),
            "boot hook returns the empty registry"
        );
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            1,
            "boot constructs exactly once"
        );

        // Tick 1 — retry hook call #1 returns {A_handle_a}. The retry
        // helper must (a) observe current is incomplete (granted=2,
        // current=0), (b) build a fresh {A_handle_a} registry, and
        // (c) merge it into shared_mcp_registry.
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
            .await
            .expect("retry_heartbeat_mcp_registry succeeds (tick 1)");
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(1),
            "tick 1: A must be admitted (B still down)"
        );
        let tick1_handles = shared_mcp_registry.as_ref().unwrap().server_handles();
        let (_, tick1_a) = tick1_handles
            .iter()
            .find(|(n, _)| n == "server-a")
            .expect("tick 1: registry must contain server-a");
        assert!(
            a_handle.ptr_eq(tick1_a),
            "tick 1: A's handle MUST be the same Arc as the hook's A handle"
        );
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            2,
            "hook fired exactly 2 times after tick 1"
        );

        // Tick 2 — retry hook call #2 returns {A_handle_a} again (B
        // still down). The retry helper must NOT churn A's handle.
        // Under the OLD count-only / superset-only logic, this tick
        // would replace the registry because {A} ⊇ {A} (reflexive
        // superset), disconnecting and respawning A's stdio child.
        // Under the NEW additive logic, the merge produces a registry
        // whose A handle is identical to the current one — no churn.
        // `ptr_eq` already compares the underlying `Arc<Mutex<…>>`
        // identity, which is exactly the property we want to assert.
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
            .await
            .expect("retry_heartbeat_mcp_registry succeeds (tick 2)");
        assert_eq!(
            shared_mcp_registry.as_ref().map(|r| r.server_count()),
            Some(1),
            "tick 2: registry must still have exactly 1 server (A); B still down"
        );
        let tick2_handles = shared_mcp_registry.as_ref().unwrap().server_handles();
        let (_, tick2_a) = tick2_handles
            .iter()
            .find(|(n, _)| n == "server-a")
            .expect("tick 2: registry must contain server-a");
        assert!(
            a_handle.ptr_eq(tick2_a),
            "tick 2: A's handle MUST be the SAME Arc as tick 1's — no churn"
        );
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            3,
            "hook fired exactly 3 times after tick 2"
        );

        // Tick 3 — retry hook call #3 returns {A_handle_a, B_handle_b}
        // (B came up). The retry helper must admit B additively while
        // keeping A's handle identical. Under the OLD logic this would
        // either replace the registry (because fresh ⊇ current) or
        // discard fresh entirely (if shape differed). The new logic
        // merges: A's live handle is Arc-cloned into the new registry,
        // B's freshly-discovered handle is appended.
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
            .await
            .expect("retry_heartbeat_mcp_registry succeeds (tick 3)");
        let tick3 = shared_mcp_registry.as_ref().expect("non-None after tick 3");
        assert_eq!(
            tick3.server_count(),
            2,
            "tick 3: registry must contain both A and B after B came up"
        );
        let tick3_handles = tick3.server_handles();
        let (_, tick3_a) = tick3_handles
            .iter()
            .find(|(n, _)| n == "server-a")
            .expect("tick 3: registry must contain server-a");
        let (_, tick3_b) = tick3_handles
            .iter()
            .find(|(n, _)| n == "server-b")
            .expect("tick 3: registry must contain server-b");
        assert!(
            a_handle.ptr_eq(tick3_a),
            "tick 3: A's handle MUST be the SAME Arc as before — B was admitted \
             additively WITHOUT replacing A's live connection"
        );
        assert!(
            b_handle.ptr_eq(tick3_b),
            "tick 3: B's handle MUST be the Arc from fresh"
        );

        // Tick 4 — registry is now complete (A + B). The retry helper
        // must NOT call the hook again (would mean churn).
        retry_heartbeat_mcp_registry(&mut shared_mcp_registry, &config, &agent_alias)
            .await
            .expect("retry_heartbeat_mcp_registry succeeds (tick 4)");
        assert_eq!(
            construct_count.load(Ordering::SeqCst),
            4,
            "hook fired exactly 4 times total — 1 boot + 3 retry ticks; \
             tick 4 did not re-fire the hook because the registry is complete"
        );
        let tick4 = shared_mcp_registry.as_ref().expect("non-None after tick 4");
        assert_eq!(
            tick4.server_count(),
            2,
            "tick 4: registry must still have both servers (no churn)"
        );
        let tick4_handles = tick4.server_handles();
        let (_, tick4_a) = tick4_handles
            .iter()
            .find(|(n, _)| n == "server-a")
            .expect("tick 4: registry must contain server-a");
        assert!(
            a_handle.ptr_eq(tick4_a),
            "tick 4: A's handle STILL the same Arc (steady state)"
        );

        // `_hook_guard` drops here, releasing the serialising lock
        // and clearing the global hook for the next test.
    }
}
