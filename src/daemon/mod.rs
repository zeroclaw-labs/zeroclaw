use crate::config::Config;
use anyhow::{bail, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Instant;
use tokio::task::JoinHandle;
use tokio::time::Duration;

const STATUS_FLUSH_SECONDS: u64 = 5;
const SHUTDOWN_GRACE_SECONDS: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShutdownSignal {
    CtrlC,
    SigTerm,
}

fn shutdown_reason(signal: ShutdownSignal) -> &'static str {
    match signal {
        ShutdownSignal::CtrlC => "shutdown requested (SIGINT)",
        ShutdownSignal::SigTerm => "shutdown requested (SIGTERM)",
    }
}

#[cfg(unix)]
fn shutdown_hint() -> &'static str {
    "Ctrl+C or SIGTERM to stop"
}

#[cfg(not(unix))]
fn shutdown_hint() -> &'static str {
    "Ctrl+C to stop"
}

async fn wait_for_shutdown_signal() -> Result<ShutdownSignal> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            ctrl_c = tokio::signal::ctrl_c() => {
                ctrl_c?;
                Ok(ShutdownSignal::CtrlC)
            }
            sigterm_result = sigterm.recv() => match sigterm_result {
                Some(()) => Ok(ShutdownSignal::SigTerm),
                None => bail!("SIGTERM signal stream unexpectedly closed"),
            },
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(ShutdownSignal::CtrlC)
    }
}

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
    println!("   {}", shutdown_hint());

    let signal = wait_for_shutdown_signal().await?;
    crate::health::mark_component_error("daemon", shutdown_reason(signal));
    let aborted =
        shutdown_handles_with_grace(handles, Duration::from_secs(SHUTDOWN_GRACE_SECONDS)).await;
    if aborted > 0 {
        tracing::warn!(
            aborted,
            grace_seconds = SHUTDOWN_GRACE_SECONDS,
            "Forced shutdown for daemon tasks that exceeded graceful drain window"
        );
    }

    Ok(())
}

async fn shutdown_handles_with_grace(handles: Vec<JoinHandle<()>>, grace: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + grace;
    while !handles.iter().all(JoinHandle::is_finished) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mut aborted = 0usize;
    for handle in &handles {
        if !handle.is_finished() {
            handle.abort();
            aborted += 1;
        }
    }
    for handle in handles {
        let _ = handle.await;
    }
    aborted
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
    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = crate::heartbeat::engine::HeartbeatEngine::new(
        config.heartbeat.clone(),
        config.workspace_dir.clone(),
        observer,
    );
    let delivery = heartbeat_delivery_target(&config)?;

    let interval_mins = config.heartbeat.interval_minutes.max(5);
    let dedupe_window = Duration::from_secs(u64::from(config.heartbeat.dedupe_window_minutes) * 60);
    let mut recently_executed_tasks: HashMap<String, Instant> = HashMap::new();
    let mut interval = tokio::time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

    loop {
        interval.tick().await;

        let file_tasks = engine.collect_tasks().await?;
        let candidate_tasks =
            heartbeat_tasks_for_tick(file_tasks, config.heartbeat.message.as_deref());
        let candidate_count = candidate_tasks.len();
        let tasks = apply_heartbeat_runtime_policy(
            candidate_tasks,
            config.heartbeat.max_tasks_per_tick,
            dedupe_window,
            &mut recently_executed_tasks,
            Instant::now(),
        );
        if tasks.is_empty() {
            if candidate_count > 0 {
                tracing::debug!(
                    "Heartbeat runtime policy skipped all candidate tasks (dedupe/cap active)"
                );
            }
            continue;
        }
        if tasks.len() < candidate_count {
            tracing::info!(
                selected = tasks.len(),
                candidates = candidate_count,
                max_tasks_per_tick = config.heartbeat.max_tasks_per_tick,
                dedupe_window_minutes = config.heartbeat.dedupe_window_minutes,
                "Heartbeat runtime policy filtered candidate tasks"
            );
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

fn apply_heartbeat_runtime_policy(
    tasks: Vec<String>,
    max_tasks_per_tick: usize,
    dedupe_window: Duration,
    recently_executed_tasks: &mut HashMap<String, Instant>,
    now: Instant,
) -> Vec<String> {
    if max_tasks_per_tick == 0 {
        return Vec::new();
    }

    if dedupe_window.is_zero() {
        return tasks.into_iter().take(max_tasks_per_tick).collect();
    }

    recently_executed_tasks.retain(|_, seen_at| {
        now.checked_duration_since(*seen_at)
            .unwrap_or_default()
            .lt(&dedupe_window)
    });

    let mut selected = Vec::new();
    for task in tasks {
        let dedupe_key = task.trim().to_ascii_lowercase();
        if dedupe_key.is_empty() {
            continue;
        }
        let seen_recently = recently_executed_tasks
            .get(&dedupe_key)
            .is_some_and(|seen_at| {
                now.checked_duration_since(*seen_at)
                    .unwrap_or_default()
                    .lt(&dedupe_window)
            });
        if seen_recently {
            continue;
        }

        recently_executed_tasks.insert(dedupe_key, now);
        selected.push(task);
        if selected.len() >= max_tasks_per_tick {
            break;
        }
    }
    selected
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

    #[test]
    fn shutdown_reason_for_ctrl_c_mentions_sigint() {
        assert_eq!(
            shutdown_reason(ShutdownSignal::CtrlC),
            "shutdown requested (SIGINT)"
        );
    }

    #[test]
    fn shutdown_reason_for_sigterm_mentions_sigterm() {
        assert_eq!(
            shutdown_reason(ShutdownSignal::SigTerm),
            "shutdown requested (SIGTERM)"
        );
    }

    #[test]
    fn shutdown_hint_matches_platform_signal_support() {
        #[cfg(unix)]
        assert_eq!(shutdown_hint(), "Ctrl+C or SIGTERM to stop");

        #[cfg(not(unix))]
        assert_eq!(shutdown_hint(), "Ctrl+C to stop");
    }

    #[tokio::test]
    async fn graceful_shutdown_waits_for_completed_handles_without_abort() {
        let finished = tokio::spawn(async {});
        let aborted = shutdown_handles_with_grace(vec![finished], Duration::from_millis(20)).await;
        assert_eq!(aborted, 0);
    }

    #[tokio::test]
    async fn graceful_shutdown_aborts_stuck_handles_after_timeout() {
        let never_finishes = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let started = tokio::time::Instant::now();
        let aborted =
            shutdown_handles_with_grace(vec![never_finishes], Duration::from_millis(20)).await;

        assert_eq!(aborted, 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown should not block indefinitely"
        );
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
    fn heartbeat_runtime_policy_limits_tasks_per_tick() {
        let now = Instant::now();
        let mut recent = HashMap::new();
        let tasks = apply_heartbeat_runtime_policy(
            vec![
                "task-a".to_string(),
                "task-b".to_string(),
                "task-c".to_string(),
            ],
            2,
            Duration::ZERO,
            &mut recent,
            now,
        );
        assert_eq!(tasks, vec!["task-a".to_string(), "task-b".to_string()]);
    }

    #[test]
    fn heartbeat_runtime_policy_dedupes_recent_tasks_case_insensitive() {
        let now = Instant::now();
        let mut recent = HashMap::new();
        let window = Duration::from_secs(300);

        let first = apply_heartbeat_runtime_policy(
            vec!["Task-A".to_string(), "Task-B".to_string()],
            5,
            window,
            &mut recent,
            now,
        );
        assert_eq!(first.len(), 2);

        let second = apply_heartbeat_runtime_policy(
            vec!["task-a".to_string(), "task-c".to_string()],
            5,
            window,
            &mut recent,
            now + Duration::from_secs(60),
        );
        assert_eq!(second, vec!["task-c".to_string()]);
    }

    #[test]
    fn heartbeat_runtime_policy_allows_task_after_dedupe_window() {
        let now = Instant::now();
        let mut recent = HashMap::new();
        let window = Duration::from_secs(60);

        let first =
            apply_heartbeat_runtime_policy(vec!["task-a".to_string()], 5, window, &mut recent, now);
        assert_eq!(first, vec!["task-a".to_string()]);

        let second = apply_heartbeat_runtime_policy(
            vec!["task-a".to_string()],
            5,
            window,
            &mut recent,
            now + Duration::from_secs(61),
        );
        assert_eq!(second, vec!["task-a".to_string()]);
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
}
