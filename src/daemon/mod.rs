use crate::config::Config;
use anyhow::{bail, Result};
use chrono::Utc;
use std::future::Future;
use std::path::PathBuf;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Duration;

const STATUS_FLUSH_SECONDS: u64 = 5;

/// Grace period after shutdown signal before force-aborting component tasks.
/// Allows in-flight requests and channel messages to complete.
const SHUTDOWN_GRACE_PERIOD_SECS: u64 = 15;

pub async fn run(config: Config, host: String, port: u16) -> Result<()> {
    // Pre-flight: check if port is already in use by another zeroclaw daemon
    if let Err(_e) = check_port_available(&host, port).await {
        // Port is in use - check if it's our daemon
        if is_zeroclaw_daemon_running(&host, port).await {
            tracing::info!("ZeroClaw daemon already running on {host}:{port}");
            println!("✓ ZeroClaw daemon already running on http://{host}:{port}");
            println!("  Use 'zeroclaw restart' to restart, or 'zeroclaw status' to check health.");
            return Ok(());
        }
        // Something else is using the port
        bail!(
            "Port {port} is already in use by another process. \
             Run 'lsof -i :{port}' to identify it, or use a different port."
        );
    }

    // Shutdown signal: sender stays here, receivers go to supervisors
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    if config.heartbeat.enabled {
        let _ =
            crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&config.workspace_dir)
                .await;
    }

    let mut handles: Vec<JoinHandle<()>> =
        vec![spawn_state_writer(config.clone(), shutdown_rx.clone())];

    {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        handles.push(spawn_component_supervisor(
            "gateway",
            initial_backoff,
            max_backoff,
            shutdown_rx.clone(),
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
                shutdown_rx.clone(),
                move || {
                    let cfg = channels_cfg.clone();
                    async move { Box::pin(crate::channels::start_channels(cfg)).await }
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
            shutdown_rx.clone(),
            move || {
                let cfg = heartbeat_cfg.clone();
                async move { Box::pin(run_heartbeat_worker(cfg)).await }
            },
        ));
    }

    if config.cron.enabled {
        let scheduler_cfg = config.clone();
        handles.push(spawn_component_supervisor(
            "scheduler",
            initial_backoff,
            max_backoff,
            shutdown_rx.clone(),
            move || {
                let cfg = scheduler_cfg.clone();
                async move { crate::cron::scheduler::run(cfg).await }
            },
        ));
    } else {
        crate::health::mark_component_ok("scheduler");
        tracing::info!("Cron disabled; scheduler supervisor not started");
    }

    println!("🧠 ZeroClaw daemon started");
    println!("   Gateway:  http://{host}:{port}");
    println!("   Components: gateway, channels, heartbeat, scheduler");
    #[cfg(unix)]
    println!("   Send SIGINT (Ctrl+C) or SIGTERM to stop");
    #[cfg(not(unix))]
    println!("   Press Ctrl+C to stop");

    wait_for_shutdown_signal().await?;
    tracing::info!("Shutdown signal received, starting graceful shutdown");
    crate::health::mark_component_error("daemon", "shutdown requested");

    // Signal all supervisors/state-writer to stop their loops
    if shutdown_tx.send(true).is_err() {
        tracing::warn!("Shutdown broadcast failed: no active component receivers");
    }

    // Give components a grace period to finish in-flight work
    tracing::info!(
        "Waiting up to {SHUTDOWN_GRACE_PERIOD_SECS}s for components to finish in-flight work"
    );
    let grace_deadline =
        tokio::time::Instant::now() + Duration::from_secs(SHUTDOWN_GRACE_PERIOD_SECS);
    let mut remaining = handles;

    // Poll until all handles finish or the deadline expires
    loop {
        let mut i = 0;
        while i < remaining.len() {
            if remaining[i].is_finished() {
                let handle = remaining.swap_remove(i);
                if let Err(e) = handle.await {
                    tracing::error!("Component task panicked during shutdown: {e}");
                }
            } else {
                i += 1;
            }
        }
        if remaining.is_empty() {
            tracing::info!("All components stopped cleanly");
            break;
        }
        if tokio::time::Instant::now() >= grace_deadline {
            tracing::warn!(
                "Grace period expired with {} components still running, aborting",
                remaining.len()
            );
            for handle in &remaining {
                handle.abort();
            }
            for handle in remaining {
                let _ = handle.await;
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(())
}

pub fn state_file_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("daemon_state.json")
}

/// Wait for either SIGINT (Ctrl+C) or SIGTERM (Kubernetes pod termination).
///
/// On Unix, listens for both signals concurrently and returns on whichever
/// arrives first. On non-Unix platforms, falls back to SIGINT only.
async fn wait_for_shutdown_signal() -> Result<()> {
    wait_for_shutdown_signal_inner(None).await
}

/// Inner implementation that accepts an optional readiness notifier.
///
/// When `ready` is `Some`, the sender is fired once all signal handlers have
/// been registered — this lets tests deterministically wait for handler
/// installation instead of relying on timing-based sleeps.
async fn wait_for_shutdown_signal_inner(
    ready: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())
            .map_err(|e| anyhow::anyhow!("failed to register SIGTERM handler: {e}"))?;

        // Signal that handlers are installed before blocking on signals.
        if let Some(tx) = ready {
            let _ = tx.send(());
        }

        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.map_err(|e| anyhow::anyhow!("failed to listen for SIGINT: {e}"))?;
                tracing::info!("Received SIGINT");
            }
            sig = sigterm.recv() => {
                if sig.is_none() {
                    anyhow::bail!("SIGTERM signal stream closed unexpectedly");
                }
                tracing::info!("Received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Signal readiness before blocking (no extra handlers to register).
        if let Some(tx) = ready {
            let _ = tx.send(());
        }

        tokio::signal::ctrl_c()
            .await
            .map_err(|e| anyhow::anyhow!("failed to listen for Ctrl+C: {e}"))?;
        tracing::info!("Received SIGINT");
    }
    Ok(())
}

async fn write_snapshot(path: &std::path::Path) {
    let mut json = crate::health::snapshot_json();
    if let Some(obj) = json.as_object_mut() {
        obj.insert(
            "written_at".into(),
            serde_json::json!(Utc::now().to_rfc3339()),
        );
    }
    match serde_json::to_vec_pretty(&json) {
        Ok(data) => {
            if let Err(e) = tokio::fs::write(path, data).await {
                tracing::warn!("Failed to write daemon state file {}: {e}", path.display());
            }
        }
        Err(e) => tracing::warn!("Failed to serialize daemon state snapshot: {e}"),
    }
}

fn spawn_state_writer(config: Config, mut shutdown_rx: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = state_file_path(&config);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let mut interval = tokio::time::interval(Duration::from_secs(STATUS_FLUSH_SECONDS));
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    tracing::info!("State writer stopping due to shutdown");
                    write_snapshot(&path).await;
                    break;
                }
                _ = interval.tick() => {
                    write_snapshot(&path).await;
                }
            }
        }
    })
}

fn spawn_component_supervisor<F, Fut>(
    name: &'static str,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
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
            if *shutdown_rx.borrow() {
                tracing::info!("Supervisor '{name}' stopping due to shutdown");
                break;
            }
            crate::health::mark_component_ok(name);
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    tracing::info!("Supervisor '{name}' stopping due to shutdown");
                    break;
                }
                result = run_component() => match result {
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
            }

            crate::health::bump_component_restart(name);
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    tracing::info!("Supervisor '{name}' stopping during backoff due to shutdown");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(backoff)) => {
                    // Double backoff AFTER sleeping so first error uses initial_backoff
                    backoff = backoff.saturating_mul(2).min(max_backoff);
                }
            }
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
    let delivery = heartbeat_delivery_target(&config)?;

    let interval_mins = config.heartbeat.interval_minutes.max(5);
    let mut interval = tokio::time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

    loop {
        interval.tick().await;

        let file_tasks = engine.collect_tasks().await?;
        let tasks = heartbeat_tasks_for_tick(file_tasks, config.heartbeat.message.as_deref());
        if tasks.is_empty() {
            continue;
        }

        for task in tasks {
            let prompt = format!("[Heartbeat Task] {task}");
            let temp = config.default_temperature;
            match Box::pin(crate::agent::run(
                config.clone(),
                Some(prompt),
                None,
                None,
                temp,
                vec![],
                false,
                None,
            ))
            .await
            {
                Ok(output) => {
                    crate::health::mark_component_ok("heartbeat");
                    if let Some(announcement) = heartbeat_announcement_text(&output) {
                        if let Some((channel, target)) = &delivery {
                            if let Err(e) = crate::cron::scheduler::deliver_announcement(
                                &config,
                                channel,
                                target,
                                &announcement,
                            )
                            .await
                            {
                                crate::health::mark_component_error(
                                    "heartbeat",
                                    format!("delivery failed: {e}"),
                                );
                                tracing::warn!("Heartbeat delivery failed: {e}");
                            }
                        }
                    } else {
                        tracing::debug!(
                            "Heartbeat returned sentinel (NO_REPLY/HEARTBEAT_OK); skipping delivery"
                        );
                    }
                }
                Err(e) => {
                    crate::health::mark_component_error("heartbeat", e.to_string());
                    tracing::warn!("Heartbeat task failed: {e}");
                }
            }
        }
    }
}

fn heartbeat_announcement_text(output: &str) -> Option<String> {
    if crate::cron::scheduler::is_no_reply_sentinel(output) || is_heartbeat_ok_sentinel(output) {
        return None;
    }
    if output.trim().is_empty() {
        return Some("heartbeat task executed".to_string());
    }
    Some(output.to_string())
}

fn is_heartbeat_ok_sentinel(output: &str) -> bool {
    const HEARTBEAT_OK: &str = "HEARTBEAT_OK";
    output
        .trim()
        .get(..HEARTBEAT_OK.len())
        .map(|prefix| prefix.eq_ignore_ascii_case(HEARTBEAT_OK))
        .unwrap_or(false)
}

fn heartbeat_tasks_for_tick(
    file_tasks: Vec<String>,
    fallback_message: Option<&str>,
) -> Vec<String> {
    if !file_tasks.is_empty() {
        return file_tasks;
    }

    fallback_message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(|message| vec![message.to_string()])
        .unwrap_or_default()
}

fn heartbeat_delivery_target(config: &Config) -> Result<Option<(String, String)>> {
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
        (None, None) => Ok(None),
        (Some(_), None) => anyhow::bail!("heartbeat.to is required when heartbeat.target is set"),
        (None, Some(_)) => anyhow::bail!("heartbeat.target is required when heartbeat.to is set"),
        (Some(channel), Some(target)) => {
            validate_heartbeat_channel_config(config, channel)?;
            Ok(Some((channel.to_string(), target.to_string())))
        }
    }
}

fn validate_heartbeat_channel_config(config: &Config, channel: &str) -> Result<()> {
    let normalized = channel.to_ascii_lowercase();
    match normalized.as_str() {
        "telegram" => {
            if config.channels_config.telegram.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to telegram but channels_config.telegram is not configured"
                );
            }
        }
        "discord" => {
            if config.channels_config.discord.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to discord but channels_config.discord is not configured"
                );
            }
        }
        "slack" => {
            if config.channels_config.slack.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to slack but channels_config.slack is not configured"
                );
            }
        }
        "mattermost" => {
            if config.channels_config.mattermost.is_none() {
                anyhow::bail!(
                    "heartbeat.target is set to mattermost but channels_config.mattermost is not configured"
                );
            }
        }
        "whatsapp" | "whatsapp_web" => {
            let wa = config.channels_config.whatsapp.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "heartbeat.target is set to {channel} but channels_config.whatsapp is not configured"
                )
            })?;

            if normalized == "whatsapp_web" && wa.is_cloud_config() && !wa.is_web_config() {
                anyhow::bail!(
                    "heartbeat.target is set to whatsapp_web but channels_config.whatsapp is configured for cloud mode (set session_path for web mode)"
                );
            }
        }
        other => anyhow::bail!("unsupported heartbeat.target channel: {other}"),
    }

    Ok(())
}

fn has_supervised_channels(config: &Config) -> bool {
    config
        .channels_config
        .channels_except_webhook()
        .iter()
        .any(|(_, ok)| *ok)
}

/// Check if a port is available for binding
async fn check_port_available(host: &str, port: u16) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{host}:{port}").parse()?;
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            // Successfully bound - close it and return Ok
            drop(listener);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            bail!("Port {} is already in use", port)
        }
        Err(e) => bail!("Failed to check port {}: {}", port, e),
    }
}

/// Check if a running daemon on this port is our zeroclaw daemon
async fn is_zeroclaw_daemon_running(host: &str, port: u16) -> bool {
    let url = format!("http://{}:{}/health", host, port);
    match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    // Check if response looks like our health endpoint
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        // Our health endpoint has "status" and "runtime.components"
                        json.get("status").is_some() && json.get("runtime").is_some()
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Err(_) => false,
        },
        Err(_) => false,
    }
}

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
        let (_tx, rx) = watch::channel(false);
        let handle = spawn_component_supervisor("daemon-test-fail", 1, 1, rx, || async {
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
        let (_tx, rx) = watch::channel(false);
        let handle = spawn_component_supervisor("daemon-test-exit", 1, 1, rx, || async { Ok(()) });

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
            stream_mode: crate::config::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: crate::config::ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_dingtalk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels_config.dingtalk = Some(crate::config::schema::DingTalkConfig {
            client_id: "client_id".into(),
            client_secret: "client_secret".into(),
            allowed_users: vec!["*".into()],
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_mattermost_as_supervised_channel() {
        let mut config = Config::default();
        config.channels_config.mattermost = Some(crate::config::schema::MattermostConfig {
            url: "https://mattermost.example.com".into(),
            bot_token: "token".into(),
            channel_id: Some("channel-id".into()),
            allowed_users: vec!["*".into()],
            thread_replies: Some(true),
            mention_only: Some(false),
            group_reply: None,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_qq_as_supervised_channel() {
        let mut config = Config::default();
        config.channels_config.qq = Some(crate::config::schema::QQConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            allowed_users: vec!["*".into()],
            receive_mode: crate::config::schema::QQReceiveMode::Websocket,
            environment: crate::config::schema::QQEnvironment::Production,
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn detects_nextcloud_talk_as_supervised_channel() {
        let mut config = Config::default();
        config.channels_config.nextcloud_talk = Some(crate::config::schema::NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: None,
            allowed_users: vec!["*".into()],
        });
        assert!(has_supervised_channels(&config));
    }

    #[test]
    fn heartbeat_tasks_use_file_tasks_when_available() {
        let tasks =
            heartbeat_tasks_for_tick(vec!["From file".to_string()], Some("Fallback from config"));
        assert_eq!(tasks, vec!["From file".to_string()]);
    }

    #[test]
    fn heartbeat_tasks_fall_back_to_config_message() {
        let tasks = heartbeat_tasks_for_tick(vec![], Some("  check london time  "));
        assert_eq!(tasks, vec!["check london time".to_string()]);
    }

    #[test]
    fn heartbeat_tasks_ignore_empty_fallback_message() {
        let tasks = heartbeat_tasks_for_tick(vec![], Some("   "));
        assert!(tasks.is_empty());
    }

    #[test]
    fn heartbeat_announcement_text_skips_no_reply_sentinel() {
        assert!(heartbeat_announcement_text(" NO_reply ").is_none());
    }

    #[test]
    fn heartbeat_announcement_text_skips_heartbeat_ok_sentinel() {
        assert!(heartbeat_announcement_text(" heartbeat_ok ").is_none());
    }

    #[test]
    fn heartbeat_announcement_text_skips_heartbeat_ok_prefix_case_insensitive() {
        assert!(heartbeat_announcement_text(" heArTbEaT_oK - all clear ").is_none());
    }

    #[test]
    fn heartbeat_announcement_text_uses_default_for_empty_output() {
        assert_eq!(
            heartbeat_announcement_text(" \n\t "),
            Some("heartbeat task executed".to_string())
        );
    }

    #[test]
    fn heartbeat_announcement_text_keeps_regular_output() {
        assert_eq!(
            heartbeat_announcement_text("system nominal"),
            Some("system nominal".to_string())
        );
    }

    #[test]
    fn heartbeat_delivery_target_none_when_unset() {
        let config = Config::default();
        let target = heartbeat_delivery_target(&config).unwrap();
        assert!(target.is_none());
    }

    #[test]
    fn heartbeat_delivery_target_requires_to_field() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        let err = heartbeat_delivery_target(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("heartbeat.to is required when heartbeat.target is set"));
    }

    #[test]
    fn heartbeat_delivery_target_requires_target_field() {
        let mut config = Config::default();
        config.heartbeat.to = Some("123456".into());
        let err = heartbeat_delivery_target(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("heartbeat.target is required when heartbeat.to is set"));
    }

    #[test]
    fn heartbeat_delivery_target_rejects_unsupported_channel() {
        let mut config = Config::default();
        config.heartbeat.target = Some("email".into());
        config.heartbeat.to = Some("ops@example.com".into());
        let err = heartbeat_delivery_target(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported heartbeat.target channel"));
    }

    #[test]
    fn heartbeat_delivery_target_requires_channel_configuration() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        config.heartbeat.to = Some("123456".into());
        let err = heartbeat_delivery_target(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("channels_config.telegram is not configured"));
    }

    #[test]
    fn heartbeat_delivery_target_accepts_telegram_configuration() {
        let mut config = Config::default();
        config.heartbeat.target = Some("telegram".into());
        config.heartbeat.to = Some("123456".into());
        config.channels_config.telegram = Some(crate::config::TelegramConfig {
            bot_token: "bot-token".into(),
            allowed_users: vec![],
            stream_mode: crate::config::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: crate::config::ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
        });

        let target = heartbeat_delivery_target(&config).unwrap();
        assert_eq!(target, Some(("telegram".to_string(), "123456".to_string())));
    }

    #[test]
    fn heartbeat_delivery_target_accepts_whatsapp_web_target_in_web_mode() {
        let mut config = Config::default();
        config.heartbeat.target = Some("whatsapp_web".into());
        config.heartbeat.to = Some("+15551234567".into());
        config.channels_config.whatsapp = Some(crate::config::schema::WhatsAppConfig {
            access_token: None,
            phone_number_id: None,
            verify_token: None,
            app_secret: None,
            session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["*".into()],
        });

        let target = heartbeat_delivery_target(&config).unwrap();
        assert_eq!(
            target,
            Some(("whatsapp_web".to_string(), "+15551234567".to_string()))
        );
    }

    #[test]
    fn heartbeat_delivery_target_rejects_whatsapp_web_target_in_cloud_mode() {
        let mut config = Config::default();
        config.heartbeat.target = Some("whatsapp_web".into());
        config.heartbeat.to = Some("+15551234567".into());
        config.channels_config.whatsapp = Some(crate::config::schema::WhatsAppConfig {
            access_token: Some("token".into()),
            phone_number_id: Some("123456".into()),
            verify_token: Some("verify".into()),
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["*".into()],
        });

        let err = heartbeat_delivery_target(&config).unwrap_err();
        assert!(err.to_string().contains("configured for cloud mode"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wait_for_shutdown_signal_responds_to_sigterm() {
        use std::process;

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async {
            wait_for_shutdown_signal_inner(Some(ready_tx))
                .await
                .unwrap();
        });

        // Wait until signal handlers are installed (deterministic, no sleep)
        ready_rx
            .await
            .expect("ready channel dropped before signalling");

        // Send SIGTERM to our own process
        let pid = process::id();
        let status = std::process::Command::new("kill")
            .args(["-s", "TERM", &pid.to_string()])
            .status()
            .expect("failed to send SIGTERM");
        assert!(status.success(), "kill exited unsuccessfully: {status}");

        // Should return promptly
        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("shutdown signal handler did not return in time");
        assert!(result.is_ok(), "shutdown signal task panicked");
    }

    #[tokio::test]
    async fn supervisor_stops_on_shutdown_signal() {
        let (tx, rx) = watch::channel(false);
        let handle = spawn_component_supervisor("daemon-test-shutdown", 1, 1, rx, || async {
            // Simulate a long-running component
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(())
        });

        // Give the supervisor time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send shutdown signal
        let _ = tx.send(true);

        // Supervisor should stop within a reasonable time (not 60s)
        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("supervisor did not stop after shutdown signal");
        assert!(result.is_ok(), "supervisor task panicked");
    }

    #[test]
    fn shutdown_grace_period_is_reasonable() {
        // Ensure the grace period is between 5 and 30 seconds
        // (Kubernetes default terminationGracePeriodSeconds is 30)
        assert!(
            SHUTDOWN_GRACE_PERIOD_SECS >= 5,
            "grace period too short for in-flight requests"
        );
        assert!(
            SHUTDOWN_GRACE_PERIOD_SECS <= 30,
            "grace period exceeds default Kubernetes terminationGracePeriodSeconds"
        );
    }
}
