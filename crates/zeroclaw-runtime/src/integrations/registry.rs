//! Integration catalog — schema-driven, single-loop.
//!
//! Every entry comes from a schema-side source:
//! - Channels: `ChannelsConfig::channels()` (each multi-instance V3
//!   channel field surfaces as one entry; `ChannelConfig::name()` and
//!   `ChannelConfig::desc()` are the source of display text).
//! - Toggle integrations: `Config::integration_descriptors()` (per-struct
//!   `#[integration(...)]` attribute on `BrowserConfig` /
//!   `GoogleWorkspaceConfig`, plus an inline descriptor for `cron` whose
//!   `active` reflects whether any job is configured — cron is now a
//!   `HashMap<String, CronJobDecl>` with no enable toggle struct).
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

/// Compute an AI-model integration's status from typed-family slot
/// occupancy. The registry never branches on a provider name — the
/// canonical slot list (`for_each_model_provider_slot!`) is the single
/// source of truth, and a slot is "active" iff at least one alias is
/// configured under it. Regional variants and OAuth modes that used to
/// drive richer activation predicates are now folded onto the parent
/// typed slot, so per-row activation enums are unnecessary.
fn evaluate_model_provider_activation(
    config: &Config,
    info: &zeroclaw_providers::ModelProviderInfo,
) -> IntegrationStatus {
    bool_to_status(
        config
            .providers
            .models
            .contains_model_provider_type(info.name),
    )
}

/// Returns the integration catalog computed against `config`.
///
/// Single-loop, schema-driven. Every per-row decision lives on the
/// schema-side source; this function just concatenates the iterators.
///
/// Channel discovery walks `ChannelsConfig::channels()` so each
/// channel type's `ChannelConfig::name()` and `ChannelConfig::desc()`
/// are the single source of display text — no string literal naming
/// a channel appears in this file's production path. Multi-instance
/// V3 channels are reported active when any alias is configured.
pub fn all_integrations(config: &Config) -> Vec<IntegrationEntry> {
    let channels = config
        .channels
        .channels()
        .into_iter()
        .map(|(handle, active)| IntegrationEntry {
            name: handle.name().to_string(),
            description: handle.desc().to_string(),
            category: IntegrationCategory::Chat,
            status: bool_to_status(active),
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

    let providers = zeroclaw_providers::list_model_providers()
        .into_iter()
        .map(|info| {
            let status = evaluate_model_provider_activation(config, &info);
            IntegrationEntry {
                name: info.display_name.to_string(),
                description: String::new(),
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
    use zeroclaw_config::traits::ChannelConfig;

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
        // Schema-driven contract: every channel registered through
        // `ChannelsConfig::channels()` surfaces as a Chat entry whose
        // display_name and description come from the channel's
        // `ChannelConfig::name()` / `desc()` methods — no override
        // table lives here. V3 channels are HashMap<alias, XConfig>
        // (one entry per channel type at the registry level), so the
        // count must equal the number of (handle, _) pairs returned.
        let config = Config::default();
        let entries = all_integrations(&config);
        let channel_count = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::Chat)
            .count();
        let handles = config.channels.channels();
        assert_eq!(
            channel_count,
            handles.len(),
            "every ChannelsConfig::channels() handle should produce exactly one Chat entry",
        );
        for (handle, _active) in &handles {
            let entry = entries
                .iter()
                .find(|e| e.name == handle.name())
                .unwrap_or_else(|| {
                    panic!(
                        "channel {:?} ({:?}) missing from registry",
                        handle.name(),
                        handle.desc(),
                    )
                });
            assert!(
                !entry.name.is_empty(),
                "channel {:?} produced empty display name",
                handle.name(),
            );
            assert!(
                !entry.description.is_empty(),
                "channel {:?} missing description text",
                handle.name(),
            );
        }
    }

    #[test]
    fn telegram_active_when_configured() {
        let mut config = Config::default();
        config.channels.telegram.insert(
            "default".to_string(),
            TelegramConfig {
                enabled: true,
                bot_token: "123:ABC".into(),
                stream_mode: StreamMode::default(),
                draft_update_interval_ms: 1000,
                interrupt_on_new_message: false,
                mention_only: false,
                ack_reactions: None,
                proxy_url: None,
                approval_timeout_secs: 120,
                excluded_tools: vec![],
            },
        );
        let entries = all_integrations(&config);
        let display_name = <TelegramConfig as ChannelConfig>::name();
        let tg = entries.iter().find(|e| e.name == display_name).unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Active));
    }

    #[test]
    fn telegram_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let display_name = <TelegramConfig as ChannelConfig>::name();
        let tg = entries.iter().find(|e| e.name == display_name).unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Available));
    }

    #[test]
    fn imessage_active_when_configured() {
        let mut config = Config::default();
        config.channels.imessage.insert(
            "default".to_string(),
            IMessageConfig {
                enabled: true,
                excluded_tools: vec![],
            },
        );
        let entries = all_integrations(&config);
        let display_name = <IMessageConfig as ChannelConfig>::name();
        let im = entries.iter().find(|e| e.name == display_name).unwrap();
        assert!(matches!(im.status, IntegrationStatus::Active));
    }

    #[test]
    fn imessage_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let display_name = <IMessageConfig as ChannelConfig>::name();
        let im = entries.iter().find(|e| e.name == display_name).unwrap();
        assert!(matches!(im.status, IntegrationStatus::Available));
    }

    #[test]
    fn matrix_active_when_configured() {
        let mut config = Config::default();
        config.channels.matrix.insert(
            "default".to_string(),
            MatrixConfig {
                enabled: true,
                homeserver: "https://m.org".into(),
                access_token: Some("tok".into()),
                user_id: None,
                device_id: None,
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
                ack_reactions: Some(true),
                excluded_tools: vec![],
            },
        );
        let entries = all_integrations(&config);
        let display_name = <MatrixConfig as ChannelConfig>::name();
        let mx = entries.iter().find(|e| e.name == display_name).unwrap();
        assert!(matches!(mx.status, IntegrationStatus::Active));
    }

    /// Look up a toggle integration's status by its descriptor display
    /// name. Each call to `Config::integration_descriptors()` is the
    /// schema-side source of truth, so the helper resolves the entry
    /// dynamically rather than hardcoding the display string.
    fn toggle_status(config: &Config, field_filter: impl Fn(&str) -> bool) -> IntegrationStatus {
        let descriptor = config
            .integration_descriptors()
            .into_iter()
            .find(|d| field_filter(d.display_name))
            .unwrap_or_else(|| panic!("expected toggle integration descriptor not present"));
        let entries = all_integrations(config);
        let entry = entries
            .iter()
            .find(|e| e.name == descriptor.display_name)
            .unwrap_or_else(|| {
                panic!(
                    "registry missing toggle integration entry for {:?}",
                    descriptor.display_name,
                )
            });
        entry.status
    }

    #[test]
    fn browser_active_in_default_config() {
        // BrowserConfig::default() has enabled=true, so the toggle
        // should be Active in the unconfigured registry.
        let config = Config::default();
        assert!(matches!(
            toggle_status(&config, |n| n == "Browser"),
            IntegrationStatus::Active
        ));
    }

    #[test]
    fn browser_available_when_disabled() {
        let mut config = Config::default();
        config.browser.enabled = false;
        assert!(matches!(
            toggle_status(&config, |n| n == "Browser"),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn google_workspace_available_in_default_config() {
        // GoogleWorkspaceConfig defaults to enabled=false.
        let config = Config::default();
        assert!(matches!(
            toggle_status(&config, |n| n == "Google Workspace"),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn google_workspace_active_when_enabled() {
        let mut config = Config::default();
        config.google_workspace.enabled = true;
        assert!(matches!(
            toggle_status(&config, |n| n == "Google Workspace"),
            IntegrationStatus::Active
        ));
    }

    #[test]
    fn cron_available_when_no_jobs_configured() {
        let config = Config::default();
        assert!(matches!(
            toggle_status(&config, |n| n == "Cron"),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn cron_active_when_any_job_configured() {
        // Cron is HashMap<String, CronJobDecl>; the descriptor's
        // `active` reflects `!cron.is_empty()`, so a single entry
        // (default-constructed) flips the toggle to Active.
        let mut config = Config::default();
        config.cron.insert(
            "daily-digest".to_string(),
            zeroclaw_config::schema::CronJobDecl::default(),
        );
        assert!(matches!(
            toggle_status(&config, |n| n == "Cron"),
            IntegrationStatus::Active
        ));
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
    fn populated_typed_slot_activates_corresponding_ai_integration() {
        // PR-branch typed-family layout: regional variants are folded
        // onto the parent canonical slot (e.g. minimax-cn → minimax with
        // a typed `endpoint` enum on the alias entry). Activation is
        // therefore "any alias under the canonical slot" — a one-call
        // `contains_model_provider_type` check that drops the V2-era
        // `FallbackKeyMatches` predicate scaffolding.
        //
        // Drives every entry of `list_model_providers()` so adding a
        // new family later (one row in `for_each_model_provider_slot!`
        // + one display_name row here) is automatically covered.
        for info in zeroclaw_providers::list_model_providers() {
            let mut config = Config::default();
            assert!(
                config
                    .providers
                    .models
                    .ensure(info.name, "default")
                    .is_some(),
                "ModelProviderInfo {:?} must correspond to a typed slot \
                 (drift: name not in `for_each_model_provider_slot!`)",
                info.name,
            );
            let entries = all_integrations(&config);
            let integration = entries
                .iter()
                .find(|e| e.name == info.display_name)
                .unwrap_or_else(|| {
                    panic!(
                        "integration entry for {:?} (display {:?}) must exist",
                        info.name, info.display_name,
                    )
                });
            assert!(
                matches!(integration.status, IntegrationStatus::Active),
                "configuring slot {:?} must activate {:?} integration",
                info.name,
                info.display_name,
            );
        }
    }
}
