//! Trigger-source registry projected for the SOP authoring surfaces.
//!
//! The registry is the single source of truth for "what can trigger an SOP":
//! the fixed bound sources (webhook, cron, filesystem, ...) plus the walkable
//! set of inbound-capable channels. Surfaces render whatever this returns; they
//! never hardcode a channel or trigger list. When a channel kind has no
//! configured alias, its entry carries an empty `aliases` list plus the setup
//! deep-link path so the surface can route the user to channel configuration
//! instead of a dead end.

use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use zeroclaw_api::attribution::ChannelKind;

/// One configured instance of a channel kind (a `[channels.<type>.<alias>]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelAlias {
    pub alias: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_agent: Option<String>,
}

/// One inbound-capable channel kind and its configured aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelTriggerKind {
    /// `ChannelKind` snake_case name (e.g. `telegram`, `filesystem`).
    pub channel: String,
    /// Configured aliases for this channel kind; empty when none configured.
    pub aliases: Vec<ChannelAlias>,
    /// Whether at least one alias is configured. When false, the surface should
    /// link to setup via `setup_path` before the trigger can be used.
    pub configured: bool,
    /// Deep-link path into channel configuration for this kind.
    pub setup_path: String,
}

/// A single bindable field on a trigger source. Surfaces render an input for
/// each: a select when `options` is non-empty, otherwise a free-text field.
/// `multi` marks a field that accepts a list of the option values (rendered as
/// a multi-select) rather than a single value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TriggerField {
    /// Field key (e.g. `path`, `events`, `condition`).
    pub name: String,
    /// Allowed values when the field is enum-backed; empty means free text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Whether the field accepts multiple option values.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
}

impl TriggerField {
    fn text(name: &str) -> Self {
        Self {
            name: name.to_string(),
            options: Vec::new(),
            multi: false,
        }
    }
}

/// A bound (non-channel) trigger source and the fields it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BoundTriggerSource {
    /// Trigger `type` tag (e.g. `webhook`, `cron`, `filesystem`).
    pub source: String,
    /// Fields this source binds, in render order.
    pub fields: Vec<TriggerField>,
}

/// The full trigger-source registry for the authoring surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TriggerSourceRegistry {
    pub bound: Vec<BoundTriggerSource>,
    pub channels: Vec<ChannelTriggerKind>,
}

/// One configured channel instance, passed in from config so this module does
/// not depend on the config crate. The gateway/RPC layer maps
/// `Config::channels_by_alias()` into this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredChannel {
    pub channel_type: String,
    pub alias: String,
    pub enabled: bool,
    pub owning_agent: Option<String>,
}

fn setup_path_for(channel: &str) -> String {
    format!("/config/channels/{channel}")
}

/// Build the registry. `configured` is every declaratively configured channel
/// instance (from `Config::channels_by_alias()`); the bound sources are fixed.
#[must_use]
pub fn build_registry(configured: &[ConfiguredChannel]) -> TriggerSourceRegistry {
    let filesystem_events: Vec<String> = crate::sop::types::FilesystemEventKind::iter()
        .map(|k| {
            let s: &'static str = k.into();
            s.to_string()
        })
        .collect();

    let bound = vec![
        BoundTriggerSource {
            source: "webhook".to_string(),
            fields: vec![TriggerField::text("path")],
        },
        BoundTriggerSource {
            source: "cron".to_string(),
            fields: vec![TriggerField::text("expression")],
        },
        BoundTriggerSource {
            source: "mqtt".to_string(),
            fields: vec![TriggerField::text("topic"), TriggerField::text("condition")],
        },
        BoundTriggerSource {
            source: "filesystem".to_string(),
            fields: vec![
                TriggerField::text("path"),
                TriggerField {
                    name: "events".to_string(),
                    options: filesystem_events,
                    multi: true,
                },
                TriggerField::text("condition"),
            ],
        },
        BoundTriggerSource {
            source: "peripheral".to_string(),
            fields: vec![
                TriggerField::text("board"),
                TriggerField::text("signal"),
                TriggerField::text("condition"),
            ],
        },
        BoundTriggerSource {
            source: "calendar".to_string(),
            fields: vec![
                TriggerField::text("calendar_source"),
                TriggerField::text("calendar_ids"),
            ],
        },
        BoundTriggerSource {
            source: "manual".to_string(),
            fields: vec![],
        },
    ];

    let channels = ChannelKind::iter()
        .filter(|k| k.inbound_capable())
        .map(|kind| {
            let name: &'static str = kind.into();
            let aliases: Vec<ChannelAlias> = configured
                .iter()
                .filter(|c| c.channel_type == name)
                .map(|c| ChannelAlias {
                    alias: c.alias.clone(),
                    enabled: c.enabled,
                    owning_agent: c.owning_agent.clone(),
                })
                .collect();
            ChannelTriggerKind {
                configured: !aliases.is_empty(),
                setup_path: setup_path_for(name),
                channel: name.to_string(),
                aliases,
            }
        })
        .collect();

    TriggerSourceRegistry { bound, channels }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_walks_inbound_channels_and_excludes_cli() {
        let reg = build_registry(&[]);
        let names: Vec<&str> = reg.channels.iter().map(|c| c.channel.as_str()).collect();
        assert!(names.contains(&"telegram"));
        assert!(names.contains(&"filesystem"));
        assert!(names.contains(&"discord"));
        assert!(!names.contains(&"cli"));
        assert!(!names.contains(&"plugin"));
    }

    #[test]
    fn bound_sources_present_with_fields() {
        let reg = build_registry(&[]);
        let cron = reg
            .bound
            .iter()
            .find(|b| b.source == "cron")
            .expect("cron bound source");
        assert_eq!(
            cron.fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["expression"]
        );
    }

    #[test]
    fn filesystem_events_field_carries_enum_options() {
        let reg = build_registry(&[]);
        let fs = reg
            .bound
            .iter()
            .find(|b| b.source == "filesystem")
            .expect("filesystem bound source");
        let events = fs
            .fields
            .iter()
            .find(|f| f.name == "events")
            .expect("events field");
        assert!(events.multi);
        assert_eq!(
            events.options,
            vec!["created", "modified", "deleted", "renamed"]
        );
    }

    #[test]
    fn configured_aliases_flag_channel_and_unconfigured_links_setup() {
        let configured = vec![ConfiguredChannel {
            channel_type: "telegram".to_string(),
            alias: "main".to_string(),
            enabled: true,
            owning_agent: Some("assistant".to_string()),
        }];
        let reg = build_registry(&configured);
        let tg = reg
            .channels
            .iter()
            .find(|c| c.channel == "telegram")
            .expect("telegram present");
        assert!(tg.configured);
        assert_eq!(tg.aliases.len(), 1);
        assert_eq!(tg.aliases[0].alias, "main");

        let discord = reg
            .channels
            .iter()
            .find(|c| c.channel == "discord")
            .expect("discord present");
        assert!(!discord.configured);
        assert!(discord.aliases.is_empty());
        assert_eq!(discord.setup_path, "/config/channels/discord");
    }
}
