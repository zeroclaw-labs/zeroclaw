//! Build the configured channel targets section for system prompt injection.
//!
//! Returns `Some(string)` if any channels have `default_target` set, `None` otherwise.

use zeroclaw_config::schema::Config;

/// Scans a single channel-type map (`HashMap<String, T>`) for enabled instances
/// that have a `default_target` set, and appends `(composite_key, target)` pairs
/// to the given entries vector.
///
/// The composite key is `<channel_type>.<alias>` (e.g. `telegram.default`).
///
/// **Why a macro:** `ChannelsConfig` stores each channel type as a separate
/// `HashMap<String, SpecificConfig>` field — there is no common trait that
/// exposes `enabled`, `default_target`, and a type name uniformly. A macro
/// avoids duplicating the same 7-line `for` loop 9 times and ensures that
/// adding a new channel type only requires one additional invocation.
macro_rules! scan_channel_map {
    ($entries:expr, $channel_type:ident, $map:expr) => {
        for (alias, cfg) in $map {
            if cfg.enabled
                && let Some(ref t) = cfg.default_target
            {
                $entries.push((
                    format!(concat!(stringify!($channel_type), ".{}"), alias),
                    t.clone(),
                ));
            }
        }
    };
}

pub fn build_channel_targets(config: &Config) -> Option<String> {
    let mut entries: Vec<(String, String)> = Vec::new();

    scan_channel_map!(entries, telegram, &config.channels.telegram);
    scan_channel_map!(entries, discord, &config.channels.discord);
    scan_channel_map!(entries, slack, &config.channels.slack);
    scan_channel_map!(entries, mattermost, &config.channels.mattermost);
    scan_channel_map!(entries, matrix, &config.channels.matrix);
    scan_channel_map!(entries, irc, &config.channels.irc);
    scan_channel_map!(entries, signal, &config.channels.signal);
    scan_channel_map!(entries, whatsapp, &config.channels.whatsapp);
    scan_channel_map!(entries, lark, &config.channels.lark);

    if entries.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## Configured Channel Targets\n\n");
    out.push_str("When responding to the user, ALWAYS use the `channel_send` tool to deliver your final response to their configured channel. Do NOT just reply in text — invoke the tool with the composite key (e.g. `telegram.default`) as the `channel` parameter and the recipient below as the `to` parameter.\n\n");
    for (channel, target) in &entries {
        out.push_str(&format!("- {channel}: {target}\n"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zeroclaw_config::schema::{
        ChannelsConfig, Config, DiscordConfig, LarkConfig, TelegramConfig,
    };

    fn make_config() -> Config {
        Config {
            channels: ChannelsConfig::default(),
            ..Config::default()
        }
    }

    #[test]
    fn returns_none_when_no_channels_configured() {
        let config = make_config();
        assert!(build_channel_targets(&config).is_none());
    }

    #[test]
    fn returns_none_when_channels_have_no_default_target() {
        let mut config = make_config();
        let mut telegram = HashMap::new();
        telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                ..TelegramConfig::default()
            },
        );
        config.channels.telegram = telegram;
        assert!(build_channel_targets(&config).is_none());
    }

    #[test]
    fn returns_none_when_channel_disabled() {
        let mut config = make_config();
        let mut telegram = HashMap::new();
        telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: false,
                bot_token: "test-token".to_string(),
                default_target: Some("chat_123".to_string()),
                ..TelegramConfig::default()
            },
        );
        config.channels.telegram = telegram;
        assert!(build_channel_targets(&config).is_none());
    }

    #[test]
    fn returns_single_telegram_target() {
        let mut config = make_config();
        let mut telegram = HashMap::new();
        telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                default_target: Some("chat_123".to_string()),
                ..TelegramConfig::default()
            },
        );
        config.channels.telegram = telegram;

        let result = build_channel_targets(&config).unwrap();
        assert!(result.contains("## Configured Channel Targets"));
        assert!(result.contains("telegram.default: chat_123"));
    }

    #[test]
    fn returns_multiple_channels_of_different_types() {
        let mut config = make_config();

        let mut telegram = HashMap::new();
        telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                default_target: Some("tg_chat_1".to_string()),
                ..TelegramConfig::default()
            },
        );
        config.channels.telegram = telegram;

        let mut discord = HashMap::new();
        discord.insert(
            "prod".to_string(),
            DiscordConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                default_target: Some("discord_chan_2".to_string()),
                ..DiscordConfig::default()
            },
        );
        config.channels.discord = discord;

        let result = build_channel_targets(&config).unwrap();
        assert!(result.contains("telegram.default: tg_chat_1"));
        assert!(result.contains("discord.prod: discord_chan_2"));
    }

    #[test]
    fn includes_lark_channel() {
        let mut config = make_config();
        let mut lark = HashMap::new();
        lark.insert(
            "default".to_string(),
            LarkConfig {
                enabled: true,
                app_id: "test-app".to_string(),
                app_secret: "test-secret".to_string(),
                default_target: Some("lark_chat_42".to_string()),
                ..LarkConfig::default()
            },
        );
        config.channels.lark = lark;

        let result = build_channel_targets(&config).unwrap();
        assert!(result.contains("lark.default: lark_chat_42"));
    }

    #[test]
    fn output_contains_usage_instructions() {
        let mut config = make_config();
        let mut telegram = HashMap::new();
        telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                default_target: Some("chat_123".to_string()),
                ..TelegramConfig::default()
            },
        );
        config.channels.telegram = telegram;

        let result = build_channel_targets(&config).unwrap();
        assert!(result.contains("channel_send"));
        assert!(result.contains("composite key"));
    }
}
