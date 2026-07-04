//! Trigger-source registry projected for the SOP authoring surfaces.
//!
//! Walks the backend enums (`SopTriggerSource`, `ChannelKind`,
//! `FilesystemEventKind`) so editors render whatever the runtime supports
//! instead of hardcoding option lists. Served via `sops/trigger-sources`
//! (RPC) and the gateway's SOP authoring routes.

use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use zeroclaw_api::attribution::ChannelKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One configured channel instance an SOP channel trigger can bind to.
pub struct ChannelAlias {
    pub alias: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One channel type row: every inbound-capable `ChannelKind` appears,
/// configured or not, so editors can offer setup for missing ones.
pub struct ChannelTriggerKind {
    pub channel: String,
    pub aliases: Vec<ChannelAlias>,
    pub configured: bool,
    pub setup_path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Input widget hint for a trigger field.
#[serde(rename_all = "snake_case")]
pub enum TriggerFieldKind {
    /// Free-form single value (topic, path, cron expression, ...).
    #[default]
    Text,
    /// Multi-value selection; `options` carries the choices when finite.
    List,
    /// Condition expression evaluated against the trigger payload.
    Expression,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One editable field of a trigger source (e.g. `topic` for MQTT).
pub struct TriggerField {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
    #[serde(default)]
    pub kind: TriggerFieldKind,
}

impl TriggerField {
    fn text(name: &str) -> Self {
        Self {
            name: name.to_string(),
            options: Vec::new(),
            multi: false,
            kind: TriggerFieldKind::Text,
        }
    }

    fn list(name: &str) -> Self {
        Self {
            kind: TriggerFieldKind::List,
            ..Self::text(name)
        }
    }

    fn expression(name: &str) -> Self {
        Self {
            kind: TriggerFieldKind::Expression,
            ..Self::text(name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// A non-channel trigger source and the fields needed to configure it.
pub struct BoundTriggerSource {
    pub source: String,
    pub fields: Vec<TriggerField>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Everything an authoring surface needs to offer trigger configuration:
/// the full ordered source list, the bound (non-channel) sources with their
/// field shapes, and per-channel availability. `Channel` is special-cased
/// into `channels` because its options depend on live config, not just the
/// enum; `sources` still names it so surfaces render the picker from one
/// backend-walked list instead of reconstructing it.
pub struct TriggerSourceRegistry {
    pub sources: Vec<String>,
    pub bound: Vec<BoundTriggerSource>,
    pub channels: Vec<ChannelTriggerKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Config-derived channel instance used as input to `build_registry`;
/// decoupled from `zeroclaw_config` so the walk is testable without a full
/// `Config`.
pub struct ConfiguredChannel {
    pub channel_type: String,
    pub alias: String,
    pub enabled: bool,
    pub owning_agent: Option<String>,
}

fn setup_path_for(channel: &str) -> String {
    format!("/config/channels/{channel}")
}

/// Build the registry from live config: collects configured channel aliases
/// (normalizing `-` to `_` to match `ChannelKind` wire names) and delegates
/// to `build_registry`.
#[must_use]
pub fn registry_from_config(config: &zeroclaw_config::schema::Config) -> TriggerSourceRegistry {
    let configured: Vec<ConfiguredChannel> = config
        .channels_by_alias()
        .into_iter()
        .map(|info| ConfiguredChannel {
            channel_type: info.channel_type.replace('-', "_"),
            alias: info.alias,
            enabled: info.enabled,
            owning_agent: info.owning_agent,
        })
        .collect();
    build_registry(&configured)
}

/// Build the registry by walking the trigger-source and channel enums.
/// Every `SopTriggerSource` except `Channel` becomes a bound source; every
/// inbound-capable `ChannelKind` becomes a channel row with whatever
/// configured aliases match it.
#[must_use]
pub fn build_registry(configured: &[ConfiguredChannel]) -> TriggerSourceRegistry {
    use crate::sop::types::SopTriggerSource;

    let filesystem_events: Vec<String> = crate::sop::types::FilesystemEventKind::iter()
        .map(|k| {
            let s: &'static str = k.into();
            s.to_string()
        })
        .collect();

    let bound = SopTriggerSource::iter()
        .filter_map(|source| {
            let fields = match source {
                SopTriggerSource::Channel => return None,
                SopTriggerSource::Webhook => vec![TriggerField::text("path")],
                SopTriggerSource::Cron => vec![TriggerField::text("expression")],
                SopTriggerSource::Mqtt => vec![
                    TriggerField::text("topic"),
                    TriggerField::expression("condition"),
                ],
                SopTriggerSource::Filesystem => vec![
                    TriggerField::text("path"),
                    TriggerField {
                        name: "events".to_string(),
                        options: filesystem_events.clone(),
                        multi: true,
                        kind: TriggerFieldKind::List,
                    },
                    TriggerField::expression("condition"),
                ],
                SopTriggerSource::Peripheral => vec![
                    TriggerField::text("board"),
                    TriggerField::text("signal"),
                    TriggerField::expression("condition"),
                ],
                SopTriggerSource::Calendar => vec![
                    TriggerField::text("calendar_source"),
                    TriggerField::list("calendar_ids"),
                ],
                SopTriggerSource::Amqp => vec![
                    TriggerField::text("routing_key"),
                    TriggerField::expression("condition"),
                ],
                SopTriggerSource::Manual => vec![],
            };
            Some(BoundTriggerSource {
                source: source.to_string(),
                fields,
            })
        })
        .collect();

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

    let sources = SopTriggerSource::iter().map(|s| s.to_string()).collect();

    TriggerSourceRegistry {
        sources,
        bound,
        channels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::SopTriggerSource;
    use strum::IntoEnumIterator;

    fn configured(channel_type: &str, alias: &str, enabled: bool) -> ConfiguredChannel {
        ConfiguredChannel {
            channel_type: channel_type.to_string(),
            alias: alias.to_string(),
            enabled,
            owning_agent: None,
        }
    }

    #[test]
    fn sources_is_the_full_trigger_source_walk() {
        let registry = build_registry(&[]);
        let expected: Vec<String> = SopTriggerSource::iter().map(|s| s.to_string()).collect();
        assert_eq!(
            registry.sources, expected,
            "registry.sources must be the complete SopTriggerSource walk so \
             surfaces render the picker from it without reconstructing"
        );
        assert!(registry.sources.contains(&"channel".to_string()));
        assert!(registry.sources.contains(&"manual".to_string()));
    }

    #[test]
    fn every_non_channel_trigger_source_is_bound() {
        let registry = build_registry(&[]);
        let bound: Vec<&str> = registry.bound.iter().map(|b| b.source.as_str()).collect();
        for source in SopTriggerSource::iter() {
            if source == SopTriggerSource::Channel {
                assert!(!bound.contains(&source.to_string().as_str()));
            } else {
                assert!(
                    bound.contains(&source.to_string().as_str()),
                    "trigger source {source} missing from registry"
                );
            }
        }
    }

    #[test]
    fn filesystem_events_field_walks_the_kind_enum() {
        let registry = build_registry(&[]);
        let fs = registry
            .bound
            .iter()
            .find(|b| b.source == "filesystem")
            .unwrap();
        let events = fs.fields.iter().find(|f| f.name == "events").unwrap();
        assert!(events.multi);
        assert_eq!(events.kind, TriggerFieldKind::List);
        let expected: Vec<String> = crate::sop::types::FilesystemEventKind::iter()
            .map(|k| <&'static str>::from(k).to_string())
            .collect();
        assert_eq!(events.options, expected);
    }

    #[test]
    fn bound_field_names_match_trigger_serde_fields() {
        use crate::sop::types::SopTrigger;

        let samples = [
            SopTrigger::Mqtt {
                topic: "t".into(),
                condition: Some("c".into()),
            },
            SopTrigger::Webhook { path: "p".into() },
            SopTrigger::Cron {
                expression: "* * * * *".into(),
            },
            SopTrigger::Peripheral {
                board: "b".into(),
                signal: "s".into(),
                condition: Some("c".into()),
            },
            SopTrigger::Filesystem {
                path: "p".into(),
                events: vec![],
                condition: Some("c".into()),
            },
            SopTrigger::Calendar {
                calendar_source: "s".into(),
                calendar_ids: vec![],
            },
            SopTrigger::Manual,
            SopTrigger::Amqp {
                routing_key: "k".into(),
                condition: Some("c".into()),
            },
        ];

        let registry = build_registry(&[]);
        let mut seen_sources = Vec::new();
        for trigger in &samples {
            let source = trigger.source();
            seen_sources.push(source);
            let bound = registry
                .bound
                .iter()
                .find(|b| b.source == source.to_string())
                .unwrap_or_else(|| panic!("source {source} missing from registry"));
            let json = serde_json::to_value(trigger).unwrap();
            let keys = json.as_object().unwrap();
            for field in &bound.fields {
                assert!(
                    keys.contains_key(&field.name),
                    "registry field '{}' for source {source} does not exist on the \
                     serialized trigger; field names drifted from SopTrigger serde fields",
                    field.name
                );
            }
        }
        for source in SopTriggerSource::iter() {
            assert!(
                source == SopTriggerSource::Channel || seen_sources.contains(&source),
                "trigger source {source} has no sample in this drift guard; add one"
            );
        }
    }

    #[test]
    fn channels_walk_inbound_capable_kinds_only() {
        let registry = build_registry(&[]);
        assert!(!registry.channels.is_empty());
        for kind in ChannelKind::iter() {
            let name: &'static str = kind.into();
            let present = registry.channels.iter().any(|c| c.channel == name);
            assert_eq!(
                present,
                kind.inbound_capable(),
                "channel {name} presence must match inbound_capable"
            );
        }
    }

    #[test]
    fn configured_aliases_attach_to_their_channel_kind() {
        let registry = build_registry(&[
            configured("telegram", "prod", true),
            configured("telegram", "backup", false),
        ]);
        let telegram = registry
            .channels
            .iter()
            .find(|c| c.channel == "telegram")
            .unwrap();
        assert!(telegram.configured);
        assert_eq!(telegram.aliases.len(), 2);
        assert!(
            telegram
                .aliases
                .iter()
                .any(|a| a.alias == "prod" && a.enabled)
        );
        assert!(
            telegram
                .aliases
                .iter()
                .any(|a| a.alias == "backup" && !a.enabled)
        );

        let discord = registry
            .channels
            .iter()
            .find(|c| c.channel == "discord")
            .unwrap();
        assert!(!discord.configured);
        assert!(discord.aliases.is_empty());
        assert_eq!(discord.setup_path, "/config/channels/discord");
    }
}
