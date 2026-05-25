//! Enumerate the channel types compiled into this binary.
//!
//! Use [`compiled_channels`] in display commands (`zeroclaw channel list`) that
//! should only mention channels that can actually be started.  For a full
//! channel inventory regardless of compile-time features, use
//! [`zeroclaw_config::schema::ChannelsConfig::channels`] instead.

use zeroclaw_config::schema::ChannelsConfig;
use zeroclaw_config::traits::ChannelInfo;

// Display names from `ChannelsConfig::channels()` that are present in this
// binary. Keep this list schema-backed: tests assert every enabled name exists
// in the config crate's canonical channel inventory.
const COMPILED_CHANNEL_NAMES: &[&str] = &[
    #[cfg(feature = "channel-telegram")]
    "Telegram",
    #[cfg(feature = "channel-discord")]
    "Discord",
    #[cfg(feature = "channel-slack")]
    "Slack",
    #[cfg(feature = "channel-mattermost")]
    "Mattermost",
    #[cfg(feature = "channel-imessage")]
    "iMessage",
    #[cfg(feature = "channel-matrix")]
    "Matrix",
    #[cfg(feature = "channel-signal")]
    "Signal",
    #[cfg(feature = "channel-whatsapp-cloud")]
    "WhatsApp",
    #[cfg(feature = "whatsapp-web")]
    "WhatsApp Web",
    #[cfg(feature = "channel-linq")]
    "Linq",
    #[cfg(feature = "channel-wati")]
    "WATI",
    #[cfg(feature = "channel-nextcloud")]
    "NextCloud Talk",
    #[cfg(feature = "channel-email")]
    "Email",
    #[cfg(feature = "channel-email")]
    "Gmail Push",
    #[cfg(feature = "channel-irc")]
    "IRC",
    #[cfg(feature = "channel-lark")]
    "Lark",
    #[cfg(feature = "channel-dingtalk")]
    "DingTalk",
    #[cfg(feature = "channel-wecom")]
    "WeCom",
    #[cfg(feature = "channel-wecom-ws")]
    "WeCom WebSocket",
    #[cfg(feature = "channel-wechat")]
    "WeChat",
    #[cfg(feature = "channel-qq")]
    "QQ Official",
    #[cfg(feature = "channel-nostr")]
    "Nostr",
    #[cfg(feature = "channel-clawdtalk")]
    "ClawdTalk",
    #[cfg(feature = "channel-reddit")]
    "Reddit",
    #[cfg(feature = "channel-bluesky")]
    "Bluesky",
    #[cfg(feature = "channel-twitter")]
    "X/Twitter",
    #[cfg(feature = "channel-mochat")]
    "Mochat",
    #[cfg(feature = "channel-line")]
    "LINE",
    #[cfg(feature = "channel-voice-call")]
    "Voice Call",
    #[cfg(feature = "voice-wake")]
    "VoiceWake",
    #[cfg(feature = "channel-mqtt")]
    "MQTT",
    #[cfg(feature = "channel-webhook")]
    "Webhook",
];

/// Returns one entry per channel type compiled into this binary.
///
/// Filters the canonical channel list from [`ChannelsConfig::channels`] down to
/// only those enabled at compile time via `channel-*` / `voice-wake` feature
/// flags. Name, desc, and configured status come from the config crate's single
/// source of truth; this function contributes only the compile-time filter.
pub fn compiled_channels(cfg: &ChannelsConfig) -> Vec<ChannelInfo> {
    cfg.channels()
        .into_iter()
        .filter(|info| COMPILED_CHANNEL_NAMES.contains(&info.name))
        .collect()
}

/// Returns whether a schema channel type key is compiled into this binary.
///
/// Accepts both kebab-case keys emitted by the config schema and legacy
/// underscore spellings used in channel references.
pub fn is_channel_type_compiled(channel_type: &str) -> bool {
    match channel_type {
        "telegram" => cfg!(feature = "channel-telegram"),
        "discord" => cfg!(feature = "channel-discord"),
        "slack" => cfg!(feature = "channel-slack"),
        "mattermost" => cfg!(feature = "channel-mattermost"),
        "imessage" => cfg!(feature = "channel-imessage"),
        "matrix" => cfg!(feature = "channel-matrix"),
        "signal" => cfg!(feature = "channel-signal"),
        "whatsapp" => cfg!(feature = "channel-whatsapp-cloud"),
        "whatsapp-web" | "whatsapp_web" => cfg!(feature = "whatsapp-web"),
        "linq" => cfg!(feature = "channel-linq"),
        "wati" => cfg!(feature = "channel-wati"),
        "nextcloud-talk" | "nextcloud_talk" => cfg!(feature = "channel-nextcloud"),
        "email" | "gmail-push" | "gmail_push" => cfg!(feature = "channel-email"),
        "irc" => cfg!(feature = "channel-irc"),
        "lark" | "feishu" => cfg!(feature = "channel-lark"),
        "dingtalk" => cfg!(feature = "channel-dingtalk"),
        "wecom" => cfg!(feature = "channel-wecom"),
        "wecom-ws" | "wecom_ws" => cfg!(feature = "channel-wecom-ws"),
        "wechat" => cfg!(feature = "channel-wechat"),
        "qq" => cfg!(feature = "channel-qq"),
        "nostr" => cfg!(feature = "channel-nostr"),
        "clawdtalk" => cfg!(feature = "channel-clawdtalk"),
        "reddit" => cfg!(feature = "channel-reddit"),
        "bluesky" => cfg!(feature = "channel-bluesky"),
        "twitter" => cfg!(feature = "channel-twitter"),
        "mochat" => cfg!(feature = "channel-mochat"),
        "line" => cfg!(feature = "channel-line"),
        "voice-call" | "voice_call" => cfg!(feature = "channel-voice-call"),
        "voice-wake" | "voice_wake" => cfg!(feature = "voice-wake"),
        "mqtt" => cfg!(feature = "channel-mqtt"),
        "webhook" => cfg!(feature = "channel-webhook"),
        "acp-server" | "acp_server" => cfg!(feature = "channel-acp-server"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPILED_CHANNEL_NAMES, ChannelsConfig};
    use super::{compiled_channels, is_channel_type_compiled};
    use std::collections::BTreeSet;

    #[cfg(feature = "default-channels")]
    #[test]
    fn channel_type_compilation_tracks_enabled_features() {
        assert!(is_channel_type_compiled("telegram"));
        assert!(is_channel_type_compiled("email"));
        assert!(is_channel_type_compiled("webhook"));
        assert!(is_channel_type_compiled("acp-server"));
        assert_eq!(
            is_channel_type_compiled("nextcloud-talk"),
            cfg!(feature = "channel-nextcloud")
        );
        assert_eq!(
            is_channel_type_compiled("linq"),
            cfg!(feature = "channel-linq")
        );
    }

    #[test]
    fn compiled_channel_names_are_schema_names() {
        let cfg = ChannelsConfig::default();
        let schema_names: BTreeSet<_> = cfg.channels().into_iter().map(|info| info.name).collect();

        for name in COMPILED_CHANNEL_NAMES {
            assert!(
                schema_names.contains(name),
                "compiled channel name `{name}` is missing from ChannelsConfig::channels()"
            );
        }
    }

    #[test]
    fn compiled_channels_match_expected_schema_names() {
        let cfg = ChannelsConfig::default();
        let actual: BTreeSet<_> = compiled_channels(&cfg)
            .into_iter()
            .map(|info| info.name)
            .collect();
        let expected: BTreeSet<_> = [
            ("Telegram", cfg!(feature = "channel-telegram")),
            ("Discord", cfg!(feature = "channel-discord")),
            ("Slack", cfg!(feature = "channel-slack")),
            ("Mattermost", cfg!(feature = "channel-mattermost")),
            ("iMessage", cfg!(feature = "channel-imessage")),
            ("Matrix", cfg!(feature = "channel-matrix")),
            ("Signal", cfg!(feature = "channel-signal")),
            ("WhatsApp", cfg!(feature = "channel-whatsapp-cloud")),
            ("WhatsApp Web", cfg!(feature = "whatsapp-web")),
            ("Linq", cfg!(feature = "channel-linq")),
            ("WATI", cfg!(feature = "channel-wati")),
            ("NextCloud Talk", cfg!(feature = "channel-nextcloud")),
            ("Email", cfg!(feature = "channel-email")),
            ("Gmail Push", cfg!(feature = "channel-email")),
            ("IRC", cfg!(feature = "channel-irc")),
            ("Lark", cfg!(feature = "channel-lark")),
            ("DingTalk", cfg!(feature = "channel-dingtalk")),
            ("WeCom", cfg!(feature = "channel-wecom")),
            ("WeCom WebSocket", cfg!(feature = "channel-wecom-ws")),
            ("WeChat", cfg!(feature = "channel-wechat")),
            ("QQ Official", cfg!(feature = "channel-qq")),
            ("Nostr", cfg!(feature = "channel-nostr")),
            ("ClawdTalk", cfg!(feature = "channel-clawdtalk")),
            ("Reddit", cfg!(feature = "channel-reddit")),
            ("Bluesky", cfg!(feature = "channel-bluesky")),
            ("X/Twitter", cfg!(feature = "channel-twitter")),
            ("Mochat", cfg!(feature = "channel-mochat")),
            ("LINE", cfg!(feature = "channel-line")),
            ("Voice Call", cfg!(feature = "channel-voice-call")),
            ("VoiceWake", cfg!(feature = "voice-wake")),
            ("MQTT", cfg!(feature = "channel-mqtt")),
            ("Webhook", cfg!(feature = "channel-webhook")),
        ]
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect();

        assert_eq!(actual, expected);
    }

    #[test]
    fn compiled_channel_names_do_not_duplicate_entries() {
        let mut seen = BTreeSet::new();

        for name in COMPILED_CHANNEL_NAMES {
            assert!(
                seen.insert(*name),
                "compiled channel name `{name}` appears more than once"
            );
        }
    }
}
