//! Enumerate the channel types compiled into this binary.

use zeroclaw_config::schema::ChannelsConfig;
use zeroclaw_config::traits::ChannelInfo;

struct ChannelCompileSpec {
    /// Display name from `ChannelsConfig::channels()`, when the channel lives
    /// in the schema channel inventory. ACP is configured under `[acp]`, so it
    /// participates in type readiness without appearing in `compiled_channels`.
    schema_name: Option<&'static str>,
    /// Accepted config/API type keys. Include legacy underscore aliases where
    /// earlier channel references allowed them.
    type_keys: &'static [&'static str],
    compiled: bool,
}

// Single source of truth for both display inventory and per-config type
// readiness. Keep this schema-backed: tests assert enabled display names exist
// in the config crate's canonical channel inventory and that keys do not drift.
const CHANNEL_COMPILE_SPECS: &[ChannelCompileSpec] = &[
    ChannelCompileSpec {
        schema_name: Some("Telegram"),
        type_keys: &["telegram"],
        compiled: cfg!(feature = "channel-telegram"),
    },
    ChannelCompileSpec {
        schema_name: Some("Discord"),
        type_keys: &["discord"],
        compiled: cfg!(feature = "channel-discord"),
    },
    ChannelCompileSpec {
        schema_name: Some("Slack"),
        type_keys: &["slack"],
        compiled: cfg!(feature = "channel-slack"),
    },
    ChannelCompileSpec {
        schema_name: Some("Mattermost"),
        type_keys: &["mattermost"],
        compiled: cfg!(feature = "channel-mattermost"),
    },
    ChannelCompileSpec {
        schema_name: Some("iMessage"),
        type_keys: &["imessage"],
        compiled: cfg!(feature = "channel-imessage"),
    },
    ChannelCompileSpec {
        schema_name: Some("Matrix"),
        type_keys: &["matrix"],
        compiled: cfg!(feature = "channel-matrix"),
    },
    ChannelCompileSpec {
        schema_name: Some("Signal"),
        type_keys: &["signal"],
        compiled: cfg!(feature = "channel-signal"),
    },
    ChannelCompileSpec {
        schema_name: Some("WhatsApp"),
        type_keys: &["whatsapp"],
        compiled: cfg!(feature = "channel-whatsapp-cloud"),
    },
    ChannelCompileSpec {
        schema_name: Some("WhatsApp Web"),
        type_keys: &["whatsapp-web", "whatsapp_web"],
        compiled: cfg!(feature = "whatsapp-web"),
    },
    ChannelCompileSpec {
        schema_name: Some("Linq"),
        type_keys: &["linq"],
        compiled: cfg!(feature = "channel-linq"),
    },
    ChannelCompileSpec {
        schema_name: Some("WATI"),
        type_keys: &["wati"],
        compiled: cfg!(feature = "channel-wati"),
    },
    ChannelCompileSpec {
        schema_name: Some("NextCloud Talk"),
        type_keys: &["nextcloud", "nextcloud-talk", "nextcloud_talk"],
        compiled: cfg!(feature = "channel-nextcloud"),
    },
    ChannelCompileSpec {
        schema_name: Some("Email"),
        type_keys: &["email"],
        compiled: cfg!(feature = "channel-email"),
    },
    ChannelCompileSpec {
        schema_name: Some("Gmail Push"),
        type_keys: &["gmail-push", "gmail_push"],
        compiled: cfg!(feature = "channel-email"),
    },
    ChannelCompileSpec {
        schema_name: Some("IRC"),
        type_keys: &["irc"],
        compiled: cfg!(feature = "channel-irc"),
    },
    ChannelCompileSpec {
        schema_name: Some("Twitch"),
        type_keys: &["twitch"],
        compiled: cfg!(feature = "channel-twitch"),
    },
    ChannelCompileSpec {
        schema_name: Some("Lark"),
        type_keys: &["lark", "feishu"],
        compiled: cfg!(feature = "channel-lark"),
    },
    ChannelCompileSpec {
        schema_name: Some("DingTalk"),
        type_keys: &["dingtalk"],
        compiled: cfg!(feature = "channel-dingtalk"),
    },
    ChannelCompileSpec {
        schema_name: Some("WeCom"),
        type_keys: &["wecom"],
        compiled: cfg!(feature = "channel-wecom"),
    },
    ChannelCompileSpec {
        schema_name: Some("WeCom WebSocket"),
        type_keys: &["wecom-ws", "wecom_ws"],
        compiled: cfg!(feature = "channel-wecom-ws"),
    },
    ChannelCompileSpec {
        schema_name: Some("WeChat"),
        type_keys: &["wechat"],
        compiled: cfg!(feature = "channel-wechat"),
    },
    ChannelCompileSpec {
        schema_name: Some("QQ Official"),
        type_keys: &["qq"],
        compiled: cfg!(feature = "channel-qq"),
    },
    ChannelCompileSpec {
        schema_name: Some("Nostr"),
        type_keys: &["nostr"],
        compiled: cfg!(feature = "channel-nostr"),
    },
    ChannelCompileSpec {
        schema_name: Some("ClawdTalk"),
        type_keys: &["clawdtalk"],
        compiled: cfg!(feature = "channel-clawdtalk"),
    },
    ChannelCompileSpec {
        schema_name: Some("Reddit"),
        type_keys: &["reddit"],
        compiled: cfg!(feature = "channel-reddit"),
    },
    ChannelCompileSpec {
        schema_name: Some("Bluesky"),
        type_keys: &["bluesky"],
        compiled: cfg!(feature = "channel-bluesky"),
    },
    ChannelCompileSpec {
        schema_name: Some("Git"),
        type_keys: &["git"],
        compiled: cfg!(feature = "channel-git"),
    },
    ChannelCompileSpec {
        schema_name: Some("X/Twitter"),
        type_keys: &["twitter"],
        compiled: cfg!(feature = "channel-twitter"),
    },
    ChannelCompileSpec {
        schema_name: Some("Mochat"),
        type_keys: &["mochat"],
        compiled: cfg!(feature = "channel-mochat"),
    },
    ChannelCompileSpec {
        schema_name: Some("LINE"),
        type_keys: &["line"],
        compiled: cfg!(feature = "channel-line"),
    },
    ChannelCompileSpec {
        schema_name: Some("Voice Call"),
        type_keys: &["voice-call", "voice_call"],
        compiled: cfg!(feature = "channel-voice-call"),
    },
    ChannelCompileSpec {
        schema_name: Some("VoiceWake"),
        type_keys: &["voice-wake", "voice_wake"],
        compiled: cfg!(feature = "voice-wake"),
    },
    ChannelCompileSpec {
        schema_name: Some("MQTT"),
        type_keys: &["mqtt"],
        compiled: cfg!(feature = "channel-mqtt"),
    },
    ChannelCompileSpec {
        schema_name: Some("AMQP"),
        type_keys: &["amqp"],
        compiled: cfg!(feature = "channel-amqp"),
    },
    ChannelCompileSpec {
        schema_name: Some("Filesystem"),
        type_keys: &["filesystem"],
        compiled: cfg!(feature = "channel-filesystem"),
    },
    ChannelCompileSpec {
        schema_name: Some("Webhook"),
        type_keys: &["webhook"],
        compiled: cfg!(feature = "channel-webhook"),
    },
    ChannelCompileSpec {
        schema_name: Some("Plugin"),
        type_keys: &["plugin"],
        compiled: zeroclaw_runtime::plugin_runtime::WASM_PLUGIN_SUPPORT_COMPILED,
    },
    ChannelCompileSpec {
        schema_name: None,
        type_keys: &["acp-server", "acp_server"],
        compiled: cfg!(feature = "channel-acp-server"),
    },
];

fn compiled_channel_names() -> impl Iterator<Item = &'static str> {
    CHANNEL_COMPILE_SPECS
        .iter()
        .filter(|spec| spec.compiled)
        .filter_map(|spec| spec.schema_name)
}

pub fn compiled_channels(cfg: &ChannelsConfig) -> Vec<ChannelInfo> {
    cfg.channels()
        .into_iter()
        .filter(|info| compiled_channel_names().any(|name| name == info.name))
        .collect()
}

pub fn configured_uncompiled_channels(cfg: &ChannelsConfig) -> Vec<ChannelInfo> {
    cfg.channels()
        .into_iter()
        .filter(|info| info.configured && !is_channel_type_compiled(info.kind))
        .collect()
}

/// Returns whether a schema channel type key is compiled into this binary.
/// Accepts both kebab-case keys emitted by the config schema and legacy
/// underscore spellings used in channel references.
pub fn is_channel_type_compiled(channel_type: &str) -> bool {
    for spec in CHANNEL_COMPILE_SPECS {
        if spec.type_keys.contains(&channel_type) {
            return spec.compiled;
        }
    }
    false
}

/// Typed key over the QR-pairing channels that own persisted login/session
/// state on disk.
///
/// This is the single dispatch value for channel-owned login-state
/// operations (`crate::login_probe::persisted_login`, and any future hooks
/// over the same state). Callers resolve a channel type key to this enum
/// once via [`qr_pairing_channel`] and dispatch on the typed value; the raw
/// string key never travels past the resolution point.
///
/// Variants are feature-gated so a binary without the channel feature
/// cannot name (or dispatch on) a channel it cannot run — dispatch sites
/// stay exhaustive without a dead fallback arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrPairingChannel {
    #[cfg(feature = "channel-wechat")]
    WeChat,
    #[cfg(feature = "whatsapp-web")]
    WhatsAppWeb,
}

/// Resolve a channel type key to its typed QR-pairing channel.
///
/// This is the one place a QR-login key is parsed from a string; everything
/// downstream dispatches on [`QrPairingChannel`]. Uses the same key space
/// as [`is_channel_type_compiled`] (kebab-case schema keys plus legacy
/// underscore spellings, per `type_keys` in the compile-spec registry
/// above — `qr_pairing_keys_stay_registered_in_compile_specs` pins the
/// agreement). Returns `None` for channel types that have no channel-owned
/// QR login state and for channels not compiled into this binary — the two
/// cases callers treat identically as "unsupported".
pub fn qr_pairing_channel(channel_type: &str) -> Option<QrPairingChannel> {
    match channel_type {
        #[cfg(feature = "channel-wechat")]
        "wechat" => Some(QrPairingChannel::WeChat),
        #[cfg(feature = "whatsapp-web")]
        "whatsapp-web" | "whatsapp_web" => Some(QrPairingChannel::WhatsAppWeb),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CHANNEL_COMPILE_SPECS, ChannelsConfig};
    use super::{compiled_channels, configured_uncompiled_channels, is_channel_type_compiled};
    use std::collections::BTreeSet;

    #[cfg(feature = "default-channels")]
    #[test]
    fn channel_type_compilation_tracks_enabled_features() {
        assert!(is_channel_type_compiled("telegram"));
        assert!(is_channel_type_compiled("email"));
        assert!(is_channel_type_compiled("webhook"));
        assert!(is_channel_type_compiled("acp-server"));
        assert!(is_channel_type_compiled("discord"));
        assert_eq!(
            is_channel_type_compiled("nextcloud-talk"),
            cfg!(feature = "channel-nextcloud")
        );
        assert_eq!(
            is_channel_type_compiled("linq"),
            cfg!(feature = "channel-linq")
        );
        assert_eq!(
            is_channel_type_compiled("plugin"),
            zeroclaw_runtime::plugin_runtime::WASM_PLUGIN_SUPPORT_COMPILED
        );
    }

    #[test]
    fn compiled_channel_names_are_schema_names() {
        let cfg = ChannelsConfig::default();
        let schema_names: BTreeSet<_> = cfg.channels().into_iter().map(|info| info.name).collect();

        for name in CHANNEL_COMPILE_SPECS
            .iter()
            .filter_map(|spec| spec.schema_name)
        {
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
        let expected: BTreeSet<_> = CHANNEL_COMPILE_SPECS
            .iter()
            .filter(|spec| spec.compiled)
            .filter_map(|spec| spec.schema_name)
            .collect();

        assert_eq!(actual, expected);
    }

    #[test]
    fn configured_uncompiled_channels_uses_schema_config_status() {
        let mut cfg = ChannelsConfig::default();
        cfg.slack.insert(
            "default".to_string(),
            zeroclaw_config::schema::SlackConfig::default(),
        );
        cfg.plugin.insert(
            "mail".to_string(),
            zeroclaw_config::schema::PluginChannelConfig {
                package: "email-plugin".to_string(),
                enabled: true,
            },
        );

        let names: BTreeSet<_> = configured_uncompiled_channels(&cfg)
            .into_iter()
            .map(|info| info.name)
            .collect();

        assert_eq!(names.contains("Slack"), !cfg!(feature = "channel-slack"));
        assert_eq!(
            names.contains("Plugin"),
            !zeroclaw_runtime::plugin_runtime::WASM_PLUGIN_SUPPORT_COMPILED
        );
    }

    #[test]
    fn channel_type_compilation_matches_inventory_specs() {
        for spec in CHANNEL_COMPILE_SPECS {
            for key in spec.type_keys {
                assert_eq!(
                    is_channel_type_compiled(key),
                    spec.compiled,
                    "channel type key `{key}` drifted from its compile spec"
                );
            }
        }

        assert!(!is_channel_type_compiled("not-a-channel"));
    }

    #[test]
    fn channel_compile_specs_do_not_duplicate_entries() {
        let mut seen_names = BTreeSet::new();
        let mut seen_keys = BTreeSet::new();

        for spec in CHANNEL_COMPILE_SPECS {
            if let Some(name) = spec.schema_name {
                assert!(
                    seen_names.insert(name),
                    "compiled channel name `{name}` appears more than once"
                );
            }

            for key in spec.type_keys {
                assert!(
                    seen_keys.insert(*key),
                    "compiled channel type key `{key}` appears more than once"
                );
            }
        }
    }

    #[test]
    fn qr_pairing_keys_stay_registered_in_compile_specs() {
        // Every key the QR-pairing resolver accepts must exist in the
        // compile-spec registry with a matching compiled flag, so the typed
        // key space cannot drift from the canonical channel inventory.
        for (key, feature_compiled) in [
            ("wechat", cfg!(feature = "channel-wechat")),
            ("whatsapp-web", cfg!(feature = "whatsapp-web")),
            ("whatsapp_web", cfg!(feature = "whatsapp-web")),
        ] {
            assert!(
                CHANNEL_COMPILE_SPECS
                    .iter()
                    .any(|spec| spec.type_keys.contains(&key)),
                "QR-pairing key `{key}` is missing from CHANNEL_COMPILE_SPECS"
            );
            assert_eq!(
                super::qr_pairing_channel(key).is_some(),
                feature_compiled,
                "QR-pairing resolution for `{key}` drifted from its compile feature"
            );
        }

        // Channel types without channel-owned QR login state never resolve.
        assert_eq!(super::qr_pairing_channel("telegram"), None);
        assert_eq!(super::qr_pairing_channel("whatsapp"), None);
        assert_eq!(super::qr_pairing_channel("not-a-channel"), None);
    }

    #[test]
    fn channel_compile_specs_cover_schema_channel_types() {
        let out_of_scope = BTreeSet::from([
            // Voice duplex is a gateway event-stream config surface, not a
            // `zeroclaw-channels` Channel implementation with its own feature.
            "voice_duplex",
        ]);

        for channel_type in zeroclaw_config::schema::v2::V3_CHANNEL_TYPES {
            if out_of_scope.contains(channel_type) {
                continue;
            }
            let kebab = channel_type.replace('_', "-");
            assert!(
                CHANNEL_COMPILE_SPECS.iter().any(|spec| spec
                    .type_keys
                    .iter()
                    .any(|key| *key == *channel_type || *key == kebab)),
                "schema channel type `{channel_type}` is missing from CHANNEL_COMPILE_SPECS"
            );
        }
    }
}
