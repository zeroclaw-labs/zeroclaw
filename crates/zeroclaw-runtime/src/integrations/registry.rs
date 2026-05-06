//! Integration catalog.
//!
//! Every entry corresponds to either:
//! - a schema field (chat channels via `ChannelsConfig::nested_option_entries`,
//!   AI providers via `providers.fallback`), or
//! - a runtime built-in (`Shell`, `File System`, `Weather`, `Browser`, `Cron`,
//!   `Google Workspace`), or
//! - a compile-time platform fact (`cfg!(target_os = ...)`).
//!
//! There is no hand-maintained channel list. Adding a
//! `pub foo: Option<FooConfig>` field with `#[nested]` to `ChannelsConfig`
//! surfaces a `Foo` integration entry automatically; no edit here required.
//! There is also no "coming soon" status — if it is not in the schema or
//! a real built-in, it does not get listed.

use super::{IntegrationCategory, IntegrationEntry, IntegrationStatus};
use zeroclaw_config::schema::Config;
use zeroclaw_providers::ProviderActivation;

/// Map snake_case schema field names to display names. Anything not in
/// this table title-cases the snake_case name (e.g. `discord_history`
/// becomes "Discord History"). Override here when title-case looks
/// wrong (acronyms, brand casing).
fn channel_display_name(field: &str) -> String {
    match field {
        "imessage" => "iMessage".into(),
        "qq" => "QQ Official".into(),
        "irc" => "IRC".into(),
        "mqtt" => "MQTT".into(),
        "wati" => "WATI".into(),
        "wecom" => "WeCom".into(),
        "wechat" => "WeChat".into(),
        "dingtalk" => "DingTalk".into(),
        "whatsapp" => "WhatsApp".into(),
        "twitter" => "X / Twitter".into(),
        "bluesky" => "Bluesky".into(),
        "clawdtalk" => "ClawdTalk".into(),
        "voice_call" => "Voice Call".into(),
        "voice_wake" => "Voice Wake".into(),
        "voice_duplex" => "Voice Duplex".into(),
        "gmail_push" => "Gmail Push".into(),
        "nextcloud_talk" => "Nextcloud Talk".into(),
        "discord_history" => "Discord History".into(),
        other => snake_to_title(other),
    }
}

fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn bool_to_status(active: bool) -> IntegrationStatus {
    if active {
        IntegrationStatus::Active
    } else {
        IntegrationStatus::Available
    }
}

fn entry(
    name: impl Into<String>,
    description: impl Into<String>,
    category: IntegrationCategory,
    status: IntegrationStatus,
) -> IntegrationEntry {
    IntegrationEntry {
        name: name.into(),
        description: description.into(),
        category,
        status,
    }
}

/// Compute an AI-model integration's status from its `ProviderActivation`
/// strategy. The integrations registry never branches on a provider name —
/// every per-vendor decision lives on the `ProviderInfo` row in
/// `zeroclaw_providers::list_providers()`.
fn evaluate_provider_activation(
    config: &Config,
    info: &zeroclaw_providers::ProviderInfo,
) -> IntegrationStatus {
    let fallback = config.providers.fallback.as_deref();
    let active = match info.activation {
        ProviderActivation::FallbackKey => {
            fallback == Some(info.name) || info.aliases.iter().any(|a| fallback == Some(*a))
        }
        ProviderActivation::FallbackKeyWithApiKey => {
            (fallback == Some(info.name) || info.aliases.iter().any(|a| fallback == Some(*a)))
                && config
                    .providers
                    .fallback_provider()
                    .and_then(|e| e.api_key.as_ref())
                    .is_some()
        }
        ProviderActivation::ModelPrefix(prefix) => config
            .providers
            .fallback_provider()
            .and_then(|e| e.model.as_deref())
            .is_some_and(|m| m.starts_with(prefix)),
        ProviderActivation::FallbackKeyMatches(predicate) => fallback.is_some_and(predicate),
    };
    bool_to_status(active)
}

/// Returns the integration catalog computed against `config`.
pub fn all_integrations(config: &Config) -> Vec<IntegrationEntry> {
    let mut out: Vec<IntegrationEntry> = Vec::new();

    // ── Chat channels (schema-derived) ──
    // Every `#[nested] Option<XConfig>` field on `ChannelsConfig`
    // surfaces here. No hand-list, no ComingSoon stand-ins.
    out.push(entry(
        "CLI",
        "In-process interactive shell",
        IntegrationCategory::Chat,
        bool_to_status(config.channels.cli),
    ));
    for (field, is_set) in config.channels.nested_option_entries() {
        out.push(entry(
            channel_display_name(field),
            "Chat channel",
            IntegrationCategory::Chat,
            bool_to_status(is_set),
        ));
    }

    // ── AI Models (schema-derived) ──
    // Pure iteration over `zeroclaw_providers::list_providers()`. Every
    // per-vendor field — display_name, description, activation strategy
    // — lives on the `ProviderInfo` row at the schema. The registry
    // never branches on a provider name. Adding a new provider means
    // adding one row in `list_providers()`; no edit here required.
    for info in zeroclaw_providers::list_providers() {
        out.push(entry(
            info.display_name,
            info.description,
            IntegrationCategory::AiModel,
            evaluate_provider_activation(config, &info),
        ));
    }

    // ── Tools & Automation (runtime built-ins + a few schema bools) ──
    out.push(entry(
        "Browser",
        "Chrome/Chromium control",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.browser.enabled),
    ));
    out.push(entry(
        "Cron",
        "Scheduled tasks",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.cron.enabled),
    ));
    out.push(entry(
        "Google Workspace",
        "Drive, Gmail, Calendar, Sheets, Docs via gws CLI",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.google_workspace.enabled),
    ));
    out.push(entry(
        "Shell",
        "Terminal command execution",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));
    out.push(entry(
        "File System",
        "Read/write files",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));
    out.push(entry(
        "Weather",
        "Forecasts & conditions (wttr.in)",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));

    // ── Platforms (compile-time facts) ──
    out.push(entry(
        "macOS",
        "Native support + AppleScript",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "macos")),
    ));
    out.push(entry(
        "Linux",
        "Native support",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "linux")),
    ));
    out.push(entry(
        "Windows",
        "Native support (WSL2 recommended)",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "windows")),
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::Config;
    use zeroclaw_config::schema::{IMessageConfig, MatrixConfig, StreamMode, TelegramConfig};

    #[test]
    fn registry_has_entries() {
        let config = Config::default();
        let entries = all_integrations(&config);
        assert!(
            entries.len() >= 30,
            "Expected 30+ integrations, got {}",
            entries.len()
        );
    }

    #[test]
    fn all_categories_represented() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for cat in IntegrationCategory::all() {
            let count = entries.iter().filter(|e| e.category == *cat).count();
            assert!(count > 0, "Category {cat:?} has no entries");
        }
    }

    #[test]
    fn no_duplicate_names() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                seen.insert(entry.name.clone()),
                "Duplicate integration name: {}",
                entry.name
            );
        }
    }

    #[test]
    fn no_empty_names_or_descriptions() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for entry in &entries {
            assert!(!entry.name.is_empty(), "Found integration with empty name");
            assert!(
                !entry.description.is_empty(),
                "Integration '{}' has empty description",
                entry.name
            );
        }
    }

    #[test]
    fn telegram_active_when_configured() {
        let mut config = Config::default();
        config.channels.telegram = Some(TelegramConfig {
            enabled: true,
            bot_token: "123:ABC".into(),
            allowed_users: vec!["user".into()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });
        let entries = all_integrations(&config);
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Active));
    }

    #[test]
    fn telegram_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Available));
    }

    #[test]
    fn imessage_active_when_configured_and_displays_brand_casing() {
        let mut config = Config::default();
        config.channels.imessage = Some(IMessageConfig {
            enabled: true,
            allowed_contacts: vec!["*".into()],
        });
        let entries = all_integrations(&config);
        let im = entries.iter().find(|e| e.name == "iMessage").unwrap();
        assert!(matches!(im.status, IntegrationStatus::Active));
    }

    #[test]
    fn matrix_active_when_configured() {
        let mut config = Config::default();
        config.channels.matrix = Some(MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: zeroclaw_config::schema::StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            password: None,
            mention_only: false,
            approval_timeout_secs: 300,
            reply_in_thread: true,
            ack_reactions: true,
        });
        let entries = all_integrations(&config);
        let mx = entries.iter().find(|e| e.name == "Matrix").unwrap();
        assert!(matches!(mx.status, IntegrationStatus::Active));
    }

    #[test]
    fn cron_active_when_enabled() {
        let mut config = Config::default();
        config.cron.enabled = true;
        let entries = all_integrations(&config);
        let cron = entries.iter().find(|e| e.name == "Cron").unwrap();
        assert!(matches!(cron.status, IntegrationStatus::Active));
    }

    #[test]
    fn browser_active_when_enabled() {
        let mut config = Config::default();
        config.browser.enabled = true;
        let entries = all_integrations(&config);
        let browser = entries.iter().find(|e| e.name == "Browser").unwrap();
        assert!(matches!(browser.status, IntegrationStatus::Active));
    }

    #[test]
    fn shell_filesystem_weather_always_active() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for name in ["Shell", "File System", "Weather"] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert!(
                matches!(entry.status, IntegrationStatus::Active),
                "{name} should always be Active"
            );
        }
    }

    #[test]
    fn macos_active_on_macos() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let macos = entries.iter().find(|e| e.name == "macOS").unwrap();
        if cfg!(target_os = "macos") {
            assert!(matches!(macos.status, IntegrationStatus::Active));
        } else {
            assert!(matches!(macos.status, IntegrationStatus::Available));
        }
    }

    #[test]
    fn channel_list_derives_from_schema_includes_previously_missing_channels() {
        // The hand-maintained list in master only surfaced ~11 channels;
        // ChannelsConfig actually declares ~25+. This test pins the
        // schema-derived path so adding a new channel surfaces here.
        let config = Config::default();
        let entries = all_integrations(&config);
        let chat_names: std::collections::HashSet<&str> = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::Chat)
            .map(|e| e.name.as_str())
            .collect();
        for must in [
            "Telegram",
            "Discord",
            "Slack",
            "Matrix",
            "iMessage",
            "WhatsApp",
            // Previously missing from the hand-list:
            "Mattermost",
            "IRC",
            "Lark",
            "Line",
            "Feishu",
            "WeCom",
            "WeChat",
            "Reddit",
            "Bluesky",
            "MQTT",
            "Discord History",
        ] {
            assert!(
                chat_names.contains(must),
                "Chat category should include {must} (derived from schema)"
            );
        }
    }

    #[test]
    fn regional_provider_aliases_activate_expected_ai_integrations() {
        // For each multi-region family that uses
        // `ProviderActivation::FallbackKeyMatches`, configuring a
        // regional alias must mark the corresponding
        // `ProviderInfo`-derived integration entry as Active. Looks the
        // entry up by canonical name (not display_name) so display
        // copy can change without breaking the contract.
        let cases = [
            ("minimax-cn", "minimax"),
            ("glm-cn", "glm"),
            ("moonshot-intl", "moonshot"),
            ("qwen-intl", "qwen"),
            ("zai-cn", "zai"),
            ("baidu", "qianfan"),
        ];
        for (fallback_key, canonical) in cases {
            let mut config = Config::default();
            config.providers.fallback = Some(fallback_key.to_string());
            let entries = all_integrations(&config);
            let info = zeroclaw_providers::list_providers()
                .into_iter()
                .find(|p| p.name == canonical)
                .unwrap_or_else(|| {
                    panic!("ProviderInfo for canonical name {canonical:?} must exist")
                });
            let integration = entries
                .iter()
                .find(|e| e.name == info.display_name)
                .unwrap_or_else(|| {
                    panic!(
                        "integration entry for {canonical:?} (display {:?}) must exist",
                        info.display_name,
                    )
                });
            assert!(
                matches!(integration.status, IntegrationStatus::Active),
                "fallback {fallback_key:?} must activate {canonical:?} integration",
            );
        }
    }
}
