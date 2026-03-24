//! Unified config watcher task for hot-reload.
//!
//! Subscribes to a `tokio::sync::watch` channel carrying new `Arc<Config>`
//! and dispatches changes to SecurityPolicy, HotChannelConfig, WebFetchDomainRules,
//! and ChannelManager.

use crate::approval::ApprovalManager;
use crate::channels::manager::{ChannelChange, ChannelManager};
use crate::channels::HotChannelConfig;
use crate::config::{schema::ChannelsConfig, Config};
use crate::security::SecurityPolicy;
use crate::tools::WebFetchDomainRules;
use arc_swap::ArcSwap;
use std::sync::Arc;
use tokio::sync::watch;

/// Run the config watcher loop. Blocks until the watch sender is dropped.
pub async fn run_config_watcher(
    mut config_rx: watch::Receiver<Arc<Config>>,
    initial_config: Arc<Config>,
    security: Arc<ArcSwap<SecurityPolicy>>,
    hot_config: Arc<ArcSwap<HotChannelConfig>>,
    domain_rules: Arc<ArcSwap<WebFetchDomainRules>>,
    channel_manager: Arc<tokio::sync::Mutex<ChannelManager>>,
) {
    let mut prev_config = initial_config;

    loop {
        if config_rx.changed().await.is_err() {
            tracing::debug!("config watch sender dropped, watcher exiting");
            break;
        }
        // Debounce: drain rapid successive updates within 200ms
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while config_rx.has_changed().unwrap_or(false) {
            config_rx.borrow_and_update();
        }
        let new_config = config_rx.borrow_and_update().clone();

        // Autonomy — update SecurityPolicy + HotChannelConfig
        if new_config.autonomy != prev_config.autonomy
            || new_config.workspace_dir != prev_config.workspace_dir
        {
            let old = security.load();
            let mut new_policy =
                SecurityPolicy::from_config(&new_config.autonomy, &new_config.workspace_dir);
            new_policy.tracker = old.tracker.clone(); // Arc clone — preserves rate limit state
            security.store(Arc::new(new_policy));

            hot_config.store(Arc::new(HotChannelConfig {
                autonomy_level: new_config.autonomy.level,
                non_cli_excluded_tools: new_config.autonomy.non_cli_excluded_tools.clone(),
                approval_manager: Arc::new(ApprovalManager::for_non_interactive(
                    &new_config.autonomy,
                )),
            }));
            tracing::info!("hot-reloaded: autonomy config");
        }

        // WebFetch domains
        if new_config.web_fetch.allowed_domains != prev_config.web_fetch.allowed_domains
            || new_config.web_fetch.blocked_domains != prev_config.web_fetch.blocked_domains
        {
            domain_rules.store(Arc::new(WebFetchDomainRules {
                allowed_domains: new_config.web_fetch.allowed_domains.clone(),
                blocked_domains: new_config.web_fetch.blocked_domains.clone(),
            }));
            tracing::info!("hot-reloaded: web_fetch domain rules");
        }

        // Channels — per-channel diff across all supported channels
        let channel_diff =
            diff_channel_configs(&prev_config.channels_config, &new_config.channels_config);

        let hot_reloadable: std::collections::HashSet<&str> =
            crate::config::ChannelsConfig::hot_reloadable_channel_names()
                .iter()
                .copied()
                .collect();

        let (hot, skipped): (Vec<_>, Vec<_>) = channel_diff
            .into_iter()
            .partition(|(name, _)| hot_reloadable.contains(name.as_str()));

        if !skipped.is_empty() {
            let names: Vec<_> = skipped.iter().map(|(n, _)| n.as_str()).collect();
            tracing::warn!(
                "config changed for non-hot-reloadable channels (restart required): {}",
                names.join(", ")
            );
        }

        if !hot.is_empty() {
            let mut mgr = channel_manager.lock().await;
            if let Err(e) = mgr.reconcile_diff(&hot, &new_config).await {
                tracing::error!("channel reconcile failed: {e}");
            } else {
                tracing::info!("hot-reloaded: {} channel(s) changed", hot.len());
            }
        }

        // Advance prev_config regardless of reconcile errors.
        // On partial failure, we treat the new config as "applied" to avoid
        // re-diffing a broken state on the next update. Errors are logged above.
        prev_config = new_config;
    }
}

macro_rules! diff_all_channels {
    ($old:expr, $new:expr, $out:expr, $($field:ident),+ $(,)?) => {
        $(diff_option(stringify!($field), &$old.$field, &$new.$field, $out);)+
    };
}

/// Compare all hot-reloadable channel configs between old and new.
/// Returns a list of `(channel_name, ChannelChange)` pairs for channels that changed.
pub fn diff_channel_configs(
    old: &ChannelsConfig,
    new: &ChannelsConfig,
) -> Vec<(String, ChannelChange)> {
    let mut diffs = Vec::new();
    diff_all_channels!(
        old,
        new,
        &mut diffs,
        telegram,
        discord,
        slack,
        mattermost,
        feishu,
        dingtalk,
        wecom,
        irc,
        nextcloud_talk,
        qq,
        email,
        gmail_push,
        reddit,
        bluesky,
        twitter,
        mochat,
        wati,
        linq,
        clawdtalk,
        imessage,
        matrix,
        signal,
        whatsapp,
        lark,
        discord_history
    );
    #[cfg(feature = "channel-nostr")]
    diff_option("nostr", &old.nostr, &new.nostr, &mut diffs);
    #[cfg(feature = "voice-wake")]
    diff_option("voice_wake", &old.voice_wake, &new.voice_wake, &mut diffs);
    diffs
}

fn diff_option<T: PartialEq>(
    name: &str,
    old: &Option<T>,
    new: &Option<T>,
    out: &mut Vec<(String, ChannelChange)>,
) {
    match (old, new) {
        (None, Some(_)) => out.push((name.to_string(), ChannelChange::Added)),
        (Some(_), None) => out.push((name.to_string(), ChannelChange::Removed)),
        (Some(a), Some(b)) if a != b => out.push((name.to_string(), ChannelChange::Changed)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{StreamMode, TelegramConfig, *};
    use std::collections::HashMap;

    #[test]
    fn diff_detects_added_channel() {
        let old = ChannelsConfig::default();
        let mut new = ChannelsConfig::default();
        new.dingtalk = Some(DingTalkConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            allowed_users: vec![],
            proxy_url: None,
        });
        let diff = diff_channel_configs(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, "dingtalk");
        assert_eq!(diff[0].1, ChannelChange::Added);
    }

    #[test]
    fn diff_detects_removed_channel() {
        let mut old = ChannelsConfig::default();
        old.dingtalk = Some(DingTalkConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            allowed_users: vec![],
            proxy_url: None,
        });
        let new = ChannelsConfig::default();
        let diff = diff_channel_configs(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, "dingtalk");
        assert_eq!(diff[0].1, ChannelChange::Removed);
    }

    #[test]
    fn diff_detects_changed_channel() {
        let mut old = ChannelsConfig::default();
        old.dingtalk = Some(DingTalkConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            allowed_users: vec![],
            proxy_url: None,
        });
        let mut new = ChannelsConfig::default();
        new.dingtalk = Some(DingTalkConfig {
            client_id: "new-id".into(),
            client_secret: "secret".into(),
            allowed_users: vec![],
            proxy_url: None,
        });
        let diff = diff_channel_configs(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, "dingtalk");
        assert_eq!(diff[0].1, ChannelChange::Changed);
    }

    #[test]
    fn diff_detects_no_change() {
        let config = ChannelsConfig::default();
        let diff = diff_channel_configs(&config, &config);
        assert!(diff.is_empty());
    }

    #[test]
    fn diff_detects_telegram_added() {
        let old = ChannelsConfig::default();
        let mut new = ChannelsConfig::default();
        new.telegram = Some(TelegramConfig {
            bot_token: "token".into(),
            allowed_users: vec![],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
        });
        let diff = diff_channel_configs(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].0, "telegram");
        assert_eq!(diff[0].1, ChannelChange::Added);
    }

    #[tokio::test]
    async fn config_watcher_updates_security_on_autonomy_change() {
        use crate::approval::ApprovalManager;
        use crate::channels::manager::ChannelManager;
        use crate::channels::HotChannelConfig;
        use crate::security::SecurityPolicy;
        use crate::tools::WebFetchDomainRules;
        use arc_swap::ArcSwap;
        use std::sync::Arc;

        let config = crate::config::Config::default();
        let initial = Arc::new(config.clone());
        let (tx, rx) = tokio::sync::watch::channel(initial.clone());

        let security = Arc::new(ArcSwap::from_pointee(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        )));
        let hot = Arc::new(ArcSwap::from_pointee(HotChannelConfig {
            autonomy_level: config.autonomy.level,
            non_cli_excluded_tools: config.autonomy.non_cli_excluded_tools.clone(),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(&config.autonomy)),
        }));
        let domain_rules = Arc::new(ArcSwap::from_pointee(WebFetchDomainRules {
            allowed_domains: config.web_fetch.allowed_domains.clone(),
            blocked_domains: config.web_fetch.blocked_domains.clone(),
        }));
        let (ch_tx, _ch_rx) = tokio::sync::mpsc::channel(16);
        let mgr = Arc::new(tokio::sync::Mutex::new(ChannelManager::new(
            ch_tx,
            2,
            60,
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
        )));

        tokio::spawn(run_config_watcher(
            rx,
            initial,
            security.clone(),
            hot.clone(),
            domain_rules.clone(),
            mgr,
        ));

        // Send updated config with changed autonomy
        let mut new_config = config.clone();
        new_config.autonomy.allowed_commands = vec!["custom_cmd".into()];
        tx.send(Arc::new(new_config)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let loaded = security.load();
        assert!(loaded.allowed_commands.contains(&"custom_cmd".to_string()));
    }

    #[tokio::test]
    async fn config_watcher_updates_domain_rules_on_web_fetch_change() {
        use crate::approval::ApprovalManager;
        use crate::channels::manager::ChannelManager;
        use crate::channels::HotChannelConfig;
        use crate::security::SecurityPolicy;
        use crate::tools::WebFetchDomainRules;
        use arc_swap::ArcSwap;
        use std::sync::Arc;

        let config = crate::config::Config::default();
        let initial = Arc::new(config.clone());
        let (tx, rx) = tokio::sync::watch::channel(initial.clone());

        let security = Arc::new(ArcSwap::from_pointee(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        )));
        let hot = Arc::new(ArcSwap::from_pointee(HotChannelConfig {
            autonomy_level: config.autonomy.level,
            non_cli_excluded_tools: config.autonomy.non_cli_excluded_tools.clone(),
            approval_manager: Arc::new(ApprovalManager::for_non_interactive(&config.autonomy)),
        }));
        let domain_rules = Arc::new(ArcSwap::from_pointee(WebFetchDomainRules {
            allowed_domains: config.web_fetch.allowed_domains.clone(),
            blocked_domains: config.web_fetch.blocked_domains.clone(),
        }));
        let (ch_tx, _ch_rx) = tokio::sync::mpsc::channel(16);
        let mgr = Arc::new(tokio::sync::Mutex::new(ChannelManager::new(
            ch_tx,
            2,
            60,
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
        )));

        tokio::spawn(run_config_watcher(
            rx,
            initial,
            security.clone(),
            hot.clone(),
            domain_rules.clone(),
            mgr,
        ));

        // Send updated config with changed web_fetch domains
        let mut new_config = config.clone();
        new_config.web_fetch.blocked_domains = vec!["evil.com".into()];
        tx.send(Arc::new(new_config)).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let loaded = domain_rules.load();
        assert!(loaded.blocked_domains.contains(&"evil.com".to_string()));
    }

    #[test]
    fn matrix_is_not_hot_reloadable() {
        let hot = crate::config::ChannelsConfig::hot_reloadable_channel_names();
        assert!(!hot.contains(&"matrix"));
        assert!(!hot.contains(&"signal"));
        assert!(!hot.contains(&"whatsapp"));
    }

    #[test]
    fn telegram_is_hot_reloadable() {
        let hot = crate::config::ChannelsConfig::hot_reloadable_channel_names();
        assert!(hot.contains(&"telegram"));
        assert!(hot.contains(&"mattermost"));
    }
}
