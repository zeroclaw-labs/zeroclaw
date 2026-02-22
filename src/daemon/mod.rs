use crate::config::Config;
use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

const STATUS_FLUSH_SECONDS: u64 = 5;

pub async fn run(config: Config, host: String, port: u16) -> Result<()> {
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
    println!("   Ctrl+C to stop");

    tokio::signal::ctrl_c().await?;
    crate::health::mark_component_error("daemon", "shutdown requested");

    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await;
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

/// Maximum consecutive failures before a heartbeat task is auto-disabled
/// for the remainder of this daemon lifetime.
const HEARTBEAT_MAX_CONSECUTIVE_FAILURES: u32 = 3;

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = crate::heartbeat::engine::HeartbeatEngine::new(
        config.heartbeat.clone(),
        config.workspace_dir.clone(),
        observer,
    );

    let initial_backoff_secs = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff_secs = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff_secs);

    // Per-task failure tracking: task_description -> (consecutive_failures, last_failure_at)
    let mut failure_map: HashMap<String, (u32, Instant)> = HashMap::new();

    let interval_mins = config.heartbeat.interval_minutes.max(5);
    let mut interval = tokio::time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

    loop {
        interval.tick().await;

        let tasks = engine.collect_tasks().await?;
        if tasks.is_empty() {
            continue;
        }

        for task in tasks {
            // Check if task is permanently disabled (hit max failures)
            if let Some(&(failures, _)) = failure_map.get(&task) {
                if failures >= HEARTBEAT_MAX_CONSECUTIVE_FAILURES {
                    tracing::debug!(
                        "Heartbeat task disabled after {failures} consecutive failures, \
                         skipping: {task}"
                    );
                    continue;
                }

                // Check exponential backoff cooldown
                let backoff = initial_backoff_secs
                    .saturating_mul(1u64.checked_shl(failures).unwrap_or(u64::MAX))
                    .min(max_backoff_secs);
                let (_, last_failure_at) = failure_map[&task];
                if last_failure_at.elapsed() < Duration::from_secs(backoff) {
                    tracing::debug!("Heartbeat task in cooldown ({backoff}s), skipping: {task}");
                    continue;
                }
            }

            let prompt = format!("[Heartbeat Task] {task}");
            let temp = config.default_temperature;
            match crate::agent::run(
                config.clone(),
                Some(prompt),
                None,
                None,
                temp,
                vec![],
                false,
            )
            .await
            {
                Ok(output) => {
                    // Success: reset failure tracking for this task
                    failure_map.remove(&task);
                    crate::health::mark_component_ok("heartbeat");
                    // Deliver to configured channel (best-effort)
                    if let (Some(ch_name), Some(target)) =
                        (&config.heartbeat.channel, &config.heartbeat.target)
                    {
                        if let Err(e) = deliver_heartbeat(&config, ch_name, target, &output).await {
                            tracing::warn!("Heartbeat delivery to {ch_name} failed: {e}");
                        }
                    }
                }
                Err(e) => {
                    let (failures, _) = failure_map
                        .entry(task.clone())
                        .or_insert((0, Instant::now()));
                    *failures += 1;
                    let f = *failures;
                    // Update last_failure_at
                    failure_map.get_mut(&task).unwrap().1 = Instant::now();

                    if f >= HEARTBEAT_MAX_CONSECUTIVE_FAILURES {
                        tracing::error!(
                            "Heartbeat task disabled after {f} consecutive failures. \
                             Check HEARTBEAT.md configuration: {task} — error: {e}"
                        );
                    } else {
                        tracing::warn!(
                            "Heartbeat task failed ({f}/{HEARTBEAT_MAX_CONSECUTIVE_FAILURES}): {e}"
                        );
                    }
                    crate::health::mark_component_error("heartbeat", e.to_string());
                }
            }
        }
    }
}

async fn deliver_heartbeat(
    config: &Config,
    channel_name: &str,
    target: &str,
    output: &str,
) -> Result<()> {
    use crate::channels::{Channel, SendMessage};

    match channel_name.to_ascii_lowercase().as_str() {
        "lark" => {
            #[cfg(feature = "channel-lark")]
            {
                let lk = config
                    .channels_config
                    .lark
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("lark channel not configured"))?;
                let channel = crate::channels::LarkChannel::from_config(lk);
                channel.send(&SendMessage::new(output, target)).await?;
            }
            #[cfg(not(feature = "channel-lark"))]
            anyhow::bail!("lark channel requires the `channel-lark` build feature");
        }
        "telegram" => {
            let tg = config
                .channels_config
                .telegram
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("telegram channel not configured"))?;
            let channel = crate::channels::TelegramChannel::new(
                tg.bot_token.clone(),
                tg.allowed_users.clone(),
                tg.mention_only,
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "discord" => {
            let dc = config
                .channels_config
                .discord
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("discord channel not configured"))?;
            let channel = crate::channels::DiscordChannel::new(
                dc.bot_token.clone(),
                dc.guild_id.clone(),
                dc.allowed_users.clone(),
                dc.listen_to_bots,
                dc.mention_only,
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "slack" => {
            let sl = config
                .channels_config
                .slack
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("slack channel not configured"))?;
            let channel = crate::channels::SlackChannel::new(
                sl.bot_token.clone(),
                sl.channel_id.clone(),
                sl.allowed_users.clone(),
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        "mattermost" => {
            let mm = config
                .channels_config
                .mattermost
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("mattermost channel not configured"))?;
            let channel = crate::channels::MattermostChannel::new(
                mm.url.clone(),
                mm.bot_token.clone(),
                mm.channel_id.clone(),
                mm.allowed_users.clone(),
                mm.thread_replies.unwrap_or(true),
                mm.mention_only.unwrap_or(false),
            );
            channel.send(&SendMessage::new(output, target)).await?;
        }
        other => anyhow::bail!("unsupported heartbeat delivery channel: {other}"),
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

    #[tokio::test]
    async fn deliver_heartbeat_unsupported_channel_returns_error() {
        let config = Config::default();
        let err = deliver_heartbeat(&config, "carrier_pigeon", "target", "hello")
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported heartbeat delivery channel"));
    }

    #[tokio::test]
    async fn deliver_heartbeat_lark_not_configured_returns_error() {
        let config = Config::default();
        let err = deliver_heartbeat(&config, "lark", "oc_abc123", "report")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("lark channel not configured"));
    }

    #[tokio::test]
    async fn deliver_heartbeat_telegram_not_configured_returns_error() {
        let config = Config::default();
        let err = deliver_heartbeat(&config, "telegram", "12345", "report")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("telegram channel not configured"));
    }

    #[tokio::test]
    async fn deliver_heartbeat_case_insensitive_channel_name() {
        let config = Config::default();
        // "LARK" should match as "lark" (case insensitive), then fail because not configured
        let err = deliver_heartbeat(&config, "LARK", "oc_abc123", "report")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("lark channel not configured"))
    }
}
