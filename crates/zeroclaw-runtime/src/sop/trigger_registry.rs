//! Trigger-source registry projected for the SOP authoring surfaces.
//!

use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use zeroclaw_api::attribution::ChannelKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelAlias {
    pub alias: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelTriggerKind {
    pub channel: String,
    pub aliases: Vec<ChannelAlias>,
    pub configured: bool,
    pub setup_path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TriggerFieldKind {
    #[default]
    Text,
    List,
    Expression,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
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
pub struct BoundTriggerSource {
    pub source: String,
    pub fields: Vec<TriggerField>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TriggerSourceRegistry {
    pub bound: Vec<BoundTriggerSource>,
    pub channels: Vec<ChannelTriggerKind>,
}

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

    TriggerSourceRegistry { bound, channels }
}
