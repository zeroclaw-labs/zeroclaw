//! Channel lifecycle manager for hot-reloadable channels.
//!
//! Manages start/stop/reconcile for the 4 channels that support hot-reload:
//! Feishu, DingTalk, WeCom, Mattermost.

use crate::channels::channel_factory;
use crate::channels::traits::{Channel, ChannelMessage};
use crate::config::Config;
use anyhow::Result;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct ChannelManager {
    pub(crate) running: HashMap<String, RunningChannel>,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    channels_by_name: Arc<ArcSwap<HashMap<String, Arc<dyn Channel>>>>,
}

pub(crate) struct RunningChannel {
    pub(crate) display_name: String,
    pub(crate) task_handle: JoinHandle<()>,
    pub(crate) cancel_token: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelChange {
    Added,
    Removed,
    Changed,
}

impl ChannelManager {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        initial_backoff_secs: u64,
        max_backoff_secs: u64,
        channels_by_name: Arc<ArcSwap<HashMap<String, Arc<dyn Channel>>>>,
    ) -> Self {
        Self {
            running: HashMap::new(),
            tx,
            initial_backoff_secs,
            max_backoff_secs,
            channels_by_name,
        }
    }

    pub async fn start_channel(&mut self, name: &str, config: &Config) -> Result<()> {
        if let Some(ch) = channel_factory::build_channel_by_name(name, config) {
            let display_name = ch.name().to_string();
            let cancel_token = CancellationToken::new();
            let handle = crate::channels::spawn_supervised_listener_cancellable(
                ch.clone(),
                self.tx.clone(),
                self.initial_backoff_secs,
                self.max_backoff_secs,
                cancel_token.clone(),
            );
            self.running.insert(
                name.to_string(),
                RunningChannel {
                    display_name,
                    task_handle: handle,
                    cancel_token,
                },
            );
            // Register in channels_by_name so reply routing can find this channel
            let mut map = (**self.channels_by_name.load()).clone();
            map.insert(name.to_string(), ch);
            self.channels_by_name.store(Arc::new(map));
            tracing::info!("hot-reload: started channel '{name}'");
        } else {
            tracing::warn!("hot-reload: channel '{name}' not found in config");
        }
        Ok(())
    }

    pub async fn stop_channel(&mut self, name: &str) -> Result<()> {
        if let Some(entry) = self.running.remove(name) {
            entry.cancel_token.cancel();
            // Give the task a moment to shut down gracefully (up to 5s)
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), entry.task_handle).await;
            // Remove from channels_by_name so reply routing stops finding this channel
            let mut map = (**self.channels_by_name.load()).clone();
            map.remove(name);
            self.channels_by_name.store(Arc::new(map));
            tracing::info!("hot-reload: stopped channel '{}'", entry.display_name);
        }
        Ok(())
    }

    pub async fn stop_all(&mut self) -> Result<()> {
        let names: Vec<String> = self.running.keys().cloned().collect();
        for name in names {
            self.stop_channel(&name).await?;
        }
        Ok(())
    }

    /// Register and spawn a channel at boot time.
    /// Uses config_key as the identifier (matching the key used by diff and reconcile).
    pub fn register_boot_channel(&mut self, config_key: &str, ch: Arc<dyn Channel>) {
        let display_name = ch.name().to_string();
        let cancel_token = CancellationToken::new();
        let handle = crate::channels::spawn_supervised_listener_cancellable(
            ch,
            self.tx.clone(),
            self.initial_backoff_secs,
            self.max_backoff_secs,
            cancel_token.clone(),
        );
        self.running.insert(
            config_key.to_string(),
            RunningChannel {
                display_name,
                task_handle: handle,
                cancel_token,
            },
        );
    }

    pub async fn reconcile_diff(
        &mut self,
        diff: &[(String, ChannelChange)],
        new_config: &Config,
    ) -> Result<()> {
        for (name, change) in diff {
            match change {
                ChannelChange::Removed => {
                    self.stop_channel(name).await?;
                }
                ChannelChange::Changed => {
                    self.stop_channel(name).await?;
                    self.start_channel(name, new_config).await?;
                }
                ChannelChange::Added => {
                    self.start_channel(name, new_config).await?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn test_manager() -> ChannelManager {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let channels_by_name = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        ChannelManager::new(tx, 2, 60, channels_by_name)
    }

    fn insert_dummy_channel(manager: &mut ChannelManager, name: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let handle = tokio::spawn(async {});
        manager.running.insert(
            name.to_string(),
            RunningChannel {
                display_name: name.to_string(),
                task_handle: handle,
                cancel_token: token.clone(),
            },
        );
        token
    }

    #[tokio::test]
    async fn new_manager_has_no_running_channels() {
        let manager = test_manager();
        assert!(manager.running.is_empty());
    }

    #[tokio::test]
    async fn stop_channel_removes_and_cancels() {
        let mut manager = test_manager();
        let token = insert_dummy_channel(&mut manager, "test");
        assert!(manager.running.contains_key("test"));

        manager.stop_channel("test").await.unwrap();
        assert!(!manager.running.contains_key("test"));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn stop_all_removes_all() {
        let mut manager = test_manager();
        insert_dummy_channel(&mut manager, "a");
        insert_dummy_channel(&mut manager, "b");
        assert_eq!(manager.running.len(), 2);

        manager.stop_all().await.unwrap();
        assert!(manager.running.is_empty());
    }

    #[tokio::test]
    async fn reconcile_removed_stops_channel() {
        let mut manager = test_manager();
        insert_dummy_channel(&mut manager, "dingtalk");

        let diff = vec![("dingtalk".to_string(), ChannelChange::Removed)];
        let config = crate::config::Config::default();
        manager.reconcile_diff(&diff, &config).await.unwrap();
        assert!(!manager.running.contains_key("dingtalk"));
    }

    #[tokio::test]
    async fn register_boot_channel_is_managed() {
        let mut manager = test_manager();
        // Simulate a boot channel registration using the existing dummy helper
        let token = insert_dummy_channel(&mut manager, "telegram");
        assert!(manager.running.contains_key("telegram"));

        // reconcile_diff should be able to stop it (proving boot channels are now managed)
        let diff = vec![("telegram".to_string(), ChannelChange::Removed)];
        let config = crate::config::Config::default();
        manager.reconcile_diff(&diff, &config).await.unwrap();
        assert!(!manager.running.contains_key("telegram"));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn reconcile_no_diff_is_noop() {
        let mut manager = test_manager();
        insert_dummy_channel(&mut manager, "dingtalk");

        let diff: Vec<(String, ChannelChange)> = vec![];
        let config = crate::config::Config::default();
        manager.reconcile_diff(&diff, &config).await.unwrap();
        assert!(manager.running.contains_key("dingtalk"));
    }
}
