//! Enumerate the channel types compiled into this binary.
//!
//! Use [`compiled_channels`] in display commands (`zeroclaw channel list`) that
//! should only mention channels that can actually be started.  For a full
//! channel inventory regardless of compile-time features, use
//! [`zeroclaw_config::schema::ChannelsConfig::channels`] instead.

use zeroclaw_config::schema::ChannelsConfig;

/// A compiled channel type together with its current configuration status.
pub struct ChannelEntry {
    pub name: &'static str,
    pub desc: &'static str,
    pub configured: bool,
}

/// Returns one entry per channel type compiled into this binary.
///
/// Entries appear in the same order as the orchestrator's dispatch table.
/// Only channels enabled at compile time via `channel-*` / `voice-wake`
/// feature flags are included.
pub fn compiled_channels(cfg: &ChannelsConfig) -> Vec<ChannelEntry> {
    vec![
        #[cfg(feature = "channel-telegram")]
        ChannelEntry {
            name: "Telegram",
            desc: "connect your bot",
            configured: !cfg.telegram.is_empty(),
        },
        #[cfg(feature = "channel-discord")]
        ChannelEntry {
            name: "Discord",
            desc: "connect your bot",
            configured: !cfg.discord.is_empty(),
        },
        #[cfg(feature = "channel-slack")]
        ChannelEntry {
            name: "Slack",
            desc: "connect your bot",
            configured: !cfg.slack.is_empty(),
        },
        #[cfg(feature = "channel-mattermost")]
        ChannelEntry {
            name: "Mattermost",
            desc: "connect to your bot",
            configured: !cfg.mattermost.is_empty(),
        },
        #[cfg(feature = "channel-imessage")]
        ChannelEntry {
            name: "iMessage",
            desc: "macOS only",
            configured: !cfg.imessage.is_empty(),
        },
        #[cfg(feature = "channel-matrix")]
        ChannelEntry {
            name: "Matrix",
            desc: "self-hosted chat",
            configured: !cfg.matrix.is_empty(),
        },
        #[cfg(feature = "channel-signal")]
        ChannelEntry {
            name: "Signal",
            desc: "An open-source, encrypted messaging service",
            configured: !cfg.signal.is_empty(),
        },
        #[cfg(feature = "channel-whatsapp-cloud")]
        ChannelEntry {
            name: "WhatsApp",
            desc: "Business Cloud API",
            configured: !cfg.whatsapp.is_empty(),
        },
        #[cfg(feature = "channel-linq")]
        ChannelEntry {
            name: "Linq",
            desc: "iMessage/RCS/SMS via Linq API",
            configured: !cfg.linq.is_empty(),
        },
        #[cfg(feature = "channel-wati")]
        ChannelEntry {
            name: "WATI",
            desc: "WhatsApp via WATI Business API",
            configured: !cfg.wati.is_empty(),
        },
        #[cfg(feature = "channel-nextcloud")]
        ChannelEntry {
            name: "NextCloud Talk",
            desc: "NextCloud Talk platform",
            configured: !cfg.nextcloud_talk.is_empty(),
        },
        #[cfg(feature = "channel-email")]
        ChannelEntry {
            name: "Email",
            desc: "Email over IMAP/SMTP",
            configured: !cfg.email.is_empty(),
        },
        #[cfg(feature = "channel-email")]
        ChannelEntry {
            name: "Gmail Push",
            desc: "Gmail Pub/Sub push notifications",
            configured: !cfg.gmail_push.is_empty(),
        },
        #[cfg(feature = "channel-irc")]
        ChannelEntry {
            name: "IRC",
            desc: "IRC over TLS",
            configured: !cfg.irc.is_empty(),
        },
        #[cfg(feature = "channel-lark")]
        ChannelEntry {
            name: "Lark",
            desc: "Lark Bot",
            configured: !cfg.lark.is_empty(),
        },
        #[cfg(feature = "channel-dingtalk")]
        ChannelEntry {
            name: "DingTalk",
            desc: "DingTalk Stream Mode",
            configured: !cfg.dingtalk.is_empty(),
        },
        #[cfg(feature = "channel-wecom")]
        ChannelEntry {
            name: "WeCom",
            desc: "WeCom Bot Webhook",
            configured: !cfg.wecom.is_empty(),
        },
        #[cfg(feature = "channel-wechat")]
        ChannelEntry {
            name: "WeChat",
            desc: "WeChat iLink Bot",
            configured: !cfg.wechat.is_empty(),
        },
        #[cfg(feature = "channel-qq")]
        ChannelEntry {
            name: "QQ Official",
            desc: "Tencent QQ Bot",
            configured: !cfg.qq.is_empty(),
        },
        #[cfg(feature = "channel-nostr")]
        ChannelEntry {
            name: "Nostr",
            desc: "Nostr DMs",
            configured: !cfg.nostr.is_empty(),
        },
        #[cfg(feature = "channel-clawdtalk")]
        ChannelEntry {
            name: "ClawdTalk",
            desc: "ClawdTalk Channel",
            configured: !cfg.clawdtalk.is_empty(),
        },
        #[cfg(feature = "channel-reddit")]
        ChannelEntry {
            name: "Reddit",
            desc: "Reddit bot (OAuth2)",
            configured: !cfg.reddit.is_empty(),
        },
        #[cfg(feature = "channel-bluesky")]
        ChannelEntry {
            name: "Bluesky",
            desc: "AT Protocol",
            configured: !cfg.bluesky.is_empty(),
        },
        #[cfg(feature = "channel-twitter")]
        ChannelEntry {
            name: "X/Twitter",
            desc: "X/Twitter Bot via API v2",
            configured: !cfg.twitter.is_empty(),
        },
        #[cfg(feature = "channel-mochat")]
        ChannelEntry {
            name: "Mochat",
            desc: "Mochat Customer Service",
            configured: !cfg.mochat.is_empty(),
        },
        #[cfg(feature = "channel-line")]
        ChannelEntry {
            name: "LINE",
            desc: "connect your LINE bot",
            configured: !cfg.line.is_empty(),
        },
        #[cfg(feature = "channel-voice-call")]
        ChannelEntry {
            name: "Voice Call",
            desc: "outbound voice call channel",
            configured: !cfg.voice_call.is_empty(),
        },
        #[cfg(feature = "voice-wake")]
        ChannelEntry {
            name: "VoiceWake",
            desc: "voice wake word detection",
            configured: !cfg.voice_wake.is_empty(),
        },
        #[cfg(feature = "channel-mqtt")]
        ChannelEntry {
            name: "MQTT",
            desc: "MQTT SOP Listener",
            configured: !cfg.mqtt.is_empty(),
        },
        #[cfg(feature = "channel-webhook")]
        ChannelEntry {
            name: "Webhook",
            desc: "HTTP endpoint",
            configured: !cfg.webhook.is_empty(),
        },
    ]
}
