//! Integration catalog — schema-driven, single-loop.
//!
//! Every entry comes from a schema-side source:
//! - Channels: `ChannelsConfig::nested_option_entries()` (per-field
//!   `#[display_name = ...]` / `#[description = ...]` attributes).
//! - Toggle integrations: `Config::integration_descriptors()` (per-struct
//!   `#[integration(...)]` attribute on `BrowserConfig`, `CronConfig`,
//!   `GoogleWorkspaceConfig`).
//! - AI providers: `zeroclaw_providers::list_providers()` (each
//!   `ProviderInfo` row carries `display_name`, `description`, and a
//!   `ProviderActivation` strategy).
//! - Always-on built-in tools: `crate::tools::BUILTIN_TOOL_INTEGRATIONS`.
//! - Platforms: `super::platform::PLATFORMS` (compile-time `cfg!` facts).
//!
//! No string literal naming a channel, vendor, tool, or platform appears
//! in this file's production path. Adding a new integration of any kind
//! is one row in the corresponding schema source — the registry picks
//! it up automatically.

use super::platform::PLATFORMS;
use super::{IntegrationCategory, IntegrationEntry, IntegrationStatus};
use crate::tools::BUILTIN_TOOL_INTEGRATIONS;
use zeroclaw_config::schema::Config;
use zeroclaw_providers::ProviderActivation;

fn bool_to_status(active: bool) -> IntegrationStatus {
    if active {
        IntegrationStatus::Active
    } else {
        IntegrationStatus::Available
    }
}

/// Map the schema-side `#[integration(category = "...")]` label to the
/// runtime enum. The schema crate intentionally keeps the label as a
/// string to avoid taking a dependency on this crate's enum.
fn parse_category(label: &str) -> IntegrationCategory {
    match label {
        "Chat" => IntegrationCategory::Chat,
        "AiModel" => IntegrationCategory::AiModel,
        "ToolsAutomation" => IntegrationCategory::ToolsAutomation,
        "Platform" => IntegrationCategory::Platform,
        // Defensive default; the schema's `#[integration(category = ...)]`
        // attribute is the source of truth for valid labels.
        _ => IntegrationCategory::ToolsAutomation,
    }
}

/// Compute an AI-model integration's status from its `ProviderActivation`
/// strategy. The registry never branches on a provider name — every
/// per-vendor decision lives on the `ProviderInfo` row in
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
///
/// Single-loop, schema-driven. Every per-row decision lives on the
/// schema-side source; this function just concatenates the iterators.
pub fn all_integrations(config: &Config) -> Vec<IntegrationEntry> {
    let channels = config
        .channels
        .nested_option_entries()
        .into_iter()
        .map(|e| IntegrationEntry {
            name: e.display_name.to_string(),
            description: e.description.to_string(),
            category: IntegrationCategory::Chat,
            status: bool_to_status(e.present),
        });

    let toggles = config
        .integration_descriptors()
        .into_iter()
        .map(|d| IntegrationEntry {
            name: d.display_name.to_string(),
            description: d.description.to_string(),
            category: parse_category(d.category),
            status: bool_to_status(d.active),
        });

    let providers = zeroclaw_providers::list_providers()
        .into_iter()
        .map(|info| {
            let status = evaluate_provider_activation(config, &info);
            IntegrationEntry {
                name: info.display_name.to_string(),
                description: info.description.to_string(),
                category: IntegrationCategory::AiModel,
                status,
            }
        });

    let builtins = BUILTIN_TOOL_INTEGRATIONS
        .iter()
        .map(|(name, desc)| IntegrationEntry {
            name: (*name).to_string(),
            description: (*desc).to_string(),
            category: IntegrationCategory::ToolsAutomation,
            status: IntegrationStatus::Active,
        });

    let platforms = PLATFORMS.iter().map(|(name, available)| IntegrationEntry {
        name: (*name).to_string(),
        description: String::new(),
        category: IntegrationCategory::Platform,
        status: bool_to_status(*available),
    });

    channels
        .chain(toggles)
        .chain(providers)
        .chain(builtins)
        .chain(platforms)
        .collect()
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
    fn channel_entries_carry_per_field_metadata_from_schema() {
        // Schema-driven contract: every Option<XConfig> on ChannelsConfig
        // surfaces as a Chat entry whose display_name and description
        // come from the field's `#[display_name = ...]` /
        // `#[description = ...]` attributes — no override table here.
        let config = Config::default();
        let entries = all_integrations(&config);
        let channel_count = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::Chat)
            .count();
        let nested_count = config.channels.nested_option_entries().len();
        assert_eq!(
            channel_count, nested_count,
            "every Option<XConfig> field should produce exactly one Chat entry",
        );
        for nested in config.channels.nested_option_entries() {
            let entry = entries
                .iter()
                .find(|e| e.name == nested.display_name)
                .unwrap_or_else(|| {
                    panic!(
                        "channel field {:?} (display {:?}) missing from registry",
                        nested.field, nested.display_name,
                    )
                });
            assert!(
                !entry.name.is_empty(),
                "channel field {:?} produced empty display name",
                nested.field
            );
            assert!(
                !entry.description.is_empty(),
                "channel field {:?} (display {:?}) missing #[description = ...] attribute",
                nested.field,
                nested.display_name,
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
        let nested = config
            .channels
            .nested_option_entries()
            .into_iter()
            .find(|e| e.field == "telegram")
            .expect("telegram field declared on ChannelsConfig");
        let tg = entries
            .iter()
            .find(|e| e.name == nested.display_name)
            .unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Active));
    }

    #[test]
    fn telegram_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let nested = config
            .channels
            .nested_option_entries()
            .into_iter()
            .find(|e| e.field == "telegram")
            .expect("telegram field declared on ChannelsConfig");
        let tg = entries
            .iter()
            .find(|e| e.name == nested.display_name)
            .unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Available));
    }

    #[test]
    fn imessage_active_when_configured() {
        let mut config = Config::default();
        config.channels.imessage = Some(IMessageConfig {
            enabled: true,
            allowed_contacts: vec!["*".into()],
        });
        let entries = all_integrations(&config);
        let nested = config
            .channels
            .nested_option_entries()
            .into_iter()
            .find(|e| e.field == "imessage")
            .expect("imessage field declared on ChannelsConfig");
        let im = entries
            .iter()
            .find(|e| e.name == nested.display_name)
            .unwrap();
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
        let nested = config
            .channels
            .nested_option_entries()
            .into_iter()
            .find(|e| e.field == "matrix")
            .expect("matrix field declared on ChannelsConfig");
        let mx = entries
            .iter()
            .find(|e| e.name == nested.display_name)
            .unwrap();
        assert!(matches!(mx.status, IntegrationStatus::Active));
    }

    #[test]
    fn cron_active_when_enabled_via_integration_descriptor() {
        let mut config = Config::default();
        config.cron.enabled = true;
        let entries = all_integrations(&config);
        let descriptor = config.cron.integration_descriptor();
        let cron = entries
            .iter()
            .find(|e| e.name == descriptor.display_name)
            .unwrap();
        assert!(matches!(cron.status, IntegrationStatus::Active));
    }

    #[test]
    fn browser_active_when_enabled_via_integration_descriptor() {
        let mut config = Config::default();
        config.browser.enabled = true;
        let entries = all_integrations(&config);
        let descriptor = config.browser.integration_descriptor();
        let browser = entries
            .iter()
            .find(|e| e.name == descriptor.display_name)
            .unwrap();
        assert!(matches!(browser.status, IntegrationStatus::Active));
    }

    #[test]
    fn builtin_tool_integrations_always_active() {
        // Drift detector: every row in BUILTIN_TOOL_INTEGRATIONS must
        // surface as an Active entry. Adding / removing a built-in is
        // the single edit point.
        let config = Config::default();
        let entries = all_integrations(&config);
        for (name, _desc) in BUILTIN_TOOL_INTEGRATIONS {
            let entry = entries
                .iter()
                .find(|e| e.name == *name)
                .unwrap_or_else(|| panic!("built-in {name:?} missing from registry"));
            assert!(
                matches!(entry.status, IntegrationStatus::Active),
                "{name} should always be Active",
            );
        }
    }

    #[test]
    fn platforms_match_compile_time_constants() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for (name, available) in PLATFORMS {
            let entry = entries
                .iter()
                .find(|e| e.name == *name)
                .unwrap_or_else(|| panic!("platform {name:?} missing from registry"));
            let expected = bool_to_status(*available);
            assert_eq!(
                entry.status, expected,
                "platform {name:?} status disagrees with PLATFORMS const",
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
