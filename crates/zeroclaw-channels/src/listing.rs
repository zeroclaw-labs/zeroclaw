//! Enumerate the channel types compiled into this binary.
//!
//! Use [`compiled_channels`] in display commands (`zeroclaw channel list`) that
//! should only mention channels that can actually be started.  For a full
//! channel inventory regardless of compile-time features, use
//! [`zeroclaw_config::schema::ChannelsConfig::channels`] instead.

use zeroclaw_config::schema::ChannelsConfig;
use zeroclaw_config::traits::ChannelInfo;

/// Returns one entry per channel type compiled into this binary.
///
/// Filters the canonical channel list from [`ChannelsConfig::channels`] down to
/// only those enabled at compile time via `channel-*` / `voice-wake` feature
/// flags. Name, desc, and configured status come from the config crate's single
/// source of truth; this function contributes only the compile-time filter.
pub fn compiled_channels(cfg: &ChannelsConfig) -> Vec<ChannelInfo> {
    // Each entry is the display name of one channel, included only when its
    // corresponding feature flag is enabled at compile time.
    let compiled: &[&str] = &[
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

    cfg.channels()
        .into_iter()
        .filter(|info| compiled.contains(&info.name))
        .collect()
}
