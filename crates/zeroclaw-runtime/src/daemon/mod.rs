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
        && let Err(e) = crate::control_plane::ControlPlaneHandle::start(&config.data_dir)
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
            match zeroclaw_memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.embedding_routes,
                config.resolve_active_storage(),
                &config.data_dir,
                None,
                Some(&config.providers.models),
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

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    use crate::heartbeat::engine::{
        HeartbeatEngine, HeartbeatTask, TaskPriority, TaskStatus, compute_adaptive_interval,
    };
    use std::sync::Arc;

    let (agent_alias, heartbeat_workspace_dir) = resolve_heartbeat_workspace_dir(&config)?;

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

        let heartbeat_memory: Option<Box<dyn zeroclaw_memory::Memory>> =
            zeroclaw_memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.embedding_routes,
                config.resolve_active_storage(),
                &config.data_dir,
                config
                    .model_provider_for_agent(&agent_alias)
                    .and_then(|e| e.api_key.as_deref()),
                Some(&config.providers.models),
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
                app_token: None,
                bot_token: None,
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
}
