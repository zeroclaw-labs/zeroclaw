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
    /// Condition payload contract for a channel-message trigger. Channel
    /// payloads are arbitrary, so this is the `open` contract; carried per row
    /// so the builder reads one shape whether the source is bound or a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<PayloadContract>,
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
    pub(crate) fn text(name: &str) -> Self {
        Self {
            name: name.to_string(),
            options: Vec::new(),
            multi: false,
            kind: TriggerFieldKind::Text,
        }
    }

    pub(crate) fn list(name: &str) -> Self {
        Self {
            kind: TriggerFieldKind::List,
            ..Self::text(name)
        }
    }

    pub(crate) fn expression(name: &str) -> Self {
        Self {
            kind: TriggerFieldKind::Expression,
            ..Self::text(name)
        }
    }

    pub(crate) fn options(name: &str, options: Vec<String>) -> Self {
        Self {
            options,
            multi: true,
            kind: TriggerFieldKind::List,
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
    /// The payload contract a `condition` on this source may reference. Absent
    /// for sources whose triggers carry no condition (`cron`, `manual`,
    /// `webhook`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<PayloadContract>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Value-input widget hint for one condition field, so the authoring surface
/// renders the right control (text box, number, checkbox, enum select) and
/// quotes string comparands correctly.
#[serde(rename_all = "snake_case")]
pub enum ConditionValueType {
    #[default]
    String,
    Number,
    Bool,
    /// One of a finite set carried in `ConditionField::options`.
    Enum,
    /// ISO-8601 instant; rendered as a datetime control, compared as a string.
    DateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One field of a trigger payload a condition can reference. `path` is the
/// JSON path suffix after `$.` (empty for a scalar payload compared directly),
/// `label` is the human name, `value_type` drives the value control, and
/// `options` carries the choices when `value_type` is `Enum`.
pub struct ConditionField {
    pub path: String,
    pub label: String,
    #[serde(default)]
    pub value_type: ConditionValueType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// The set of payload fields a condition may reference for a given source.
/// `open` marks sources whose payload is arbitrary user JSON (MQTT, AMQP,
/// webhook body, channel message): the surface offers a guided path/op/value
/// builder plus a sample-payload probe rather than a fixed field list. `direct`
/// marks a scalar payload (peripheral signal) compared with the bare `op value`
/// form and no path.
pub struct PayloadContract {
    pub open: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ConditionField>,
}

impl ConditionField {
    fn new(path: &str, label: &str, value_type: ConditionValueType) -> Self {
        Self {
            path: path.to_string(),
            label: label.to_string(),
            value_type,
            options: Vec::new(),
        }
    }

    fn enumerated(path: &str, label: &str, options: Vec<String>) -> Self {
        Self {
            value_type: ConditionValueType::Enum,
            options,
            ..Self::new(path, label, ConditionValueType::Enum)
        }
    }
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
    /// The comparison operators every condition builder renders. Read from
    /// [`crate::sop::condition::ConditionOp`], never hand-listed by a surface.
    #[serde(default)]
    pub operators: Vec<crate::sop::condition::ConditionOpSpec>,
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

/// The condition payload contract for one trigger source. This is the single
/// authority for the fields a `condition` may reference per source; the parser,
/// evaluator, and every authoring surface agree because they all read it.
///
/// Known-shape sources (filesystem, calendar) enumerate their fields; scalar
/// sources (peripheral) mark `direct`; arbitrary-payload sources (mqtt, amqp,
/// channel) mark `open`; sources whose trigger carries no condition return
/// `None`.
fn condition_contract_for(source: crate::sop::types::SopTriggerSource) -> Option<PayloadContract> {
    use crate::sop::types::{FilesystemEventKind, SopTriggerSource};

    match source {
        SopTriggerSource::Filesystem => {
            let kinds: Vec<String> = FilesystemEventKind::iter()
                .map(|k| <&'static str>::from(k).to_string())
                .collect();
            Some(PayloadContract {
                open: false,
                direct: false,
                fields: vec![
                    ConditionField::enumerated("event", "Change kind", kinds),
                    ConditionField::new("path", "Path", ConditionValueType::String),
                ],
            })
        }
        SopTriggerSource::Calendar => Some(PayloadContract {
            open: false,
            direct: false,
            fields: vec![
                ConditionField::new("event_id", "Event ID", ConditionValueType::String),
                ConditionField::new("event_title", "Event title", ConditionValueType::String),
                ConditionField::new(
                    "expected_start",
                    "Expected start",
                    ConditionValueType::DateTime,
                ),
            ],
        }),
        SopTriggerSource::Peripheral => Some(PayloadContract {
            open: false,
            direct: true,
            fields: vec![ConditionField::new(
                "",
                "Signal value",
                ConditionValueType::Number,
            )],
        }),
        SopTriggerSource::Mqtt | SopTriggerSource::Amqp => Some(PayloadContract {
            open: true,
            direct: false,
            fields: Vec::new(),
        }),
        // Channel conditions are resolved per `ChannelKind` in
        // `channel_condition_fields`, since each channel surfaces its own
        // enumerable prop set. The blanket source-level contract is unused for
        // channels and returns nothing.
        SopTriggerSource::Channel => None,
        // Webhook, Cron, and Manual triggers carry no condition field.
        SopTriggerSource::Webhook | SopTriggerSource::Cron | SopTriggerSource::Manual => None,
    }
}

/// The git/forge event types a channel condition can match on `event_type`.
/// Mirrors the forge channel's `KNOWN_EVENT_TYPES` wire contract; the forge
/// channel test suite pins the producer side, and
/// `forge_condition_fields_cover_known_event_types` pins this projection.
const FORGE_EVENT_TYPES: &[&str] = &[
    "issue_comment.created",
    "issues.opened",
    "pull_request.opened",
    "pull_request.closed",
    "pull_request.merged",
    "pull_request_review_comment.created",
    "workflow_run.completed",
    "workflow_run.failed",
    "release.published",
];

/// The enumerated prop set a channel surfaces for SOP trigger conditions,
/// keyed off `ChannelKind`. Every inbound-capable channel returns a closed
/// field list so the authoring surface always renders a walkable picker and
/// never a free-text path box. The forge channel surfaces its event envelope;
/// every conversational channel surfaces the `ChannelMessage` envelope.
fn channel_condition_fields(kind: ChannelKind) -> PayloadContract {
    let fields = match kind {
        ChannelKind::Git => vec![
            ConditionField::enumerated(
                "event_type",
                "Event type",
                FORGE_EVENT_TYPES.iter().map(|s| (*s).to_string()).collect(),
            ),
            ConditionField::new("repo", "Repository", ConditionValueType::String),
            ConditionField::new("number", "Issue/PR number", ConditionValueType::Number),
            ConditionField::new("author.login", "Author", ConditionValueType::String),
            ConditionField::new("title", "Title", ConditionValueType::String),
            ConditionField::new("body", "Body", ConditionValueType::String),
        ],
        _ => vec![
            ConditionField::new("sender", "Sender", ConditionValueType::String),
            ConditionField::new("content", "Message text", ConditionValueType::String),
            ConditionField::new(
                "channel_alias",
                "Channel instance",
                ConditionValueType::String,
            ),
        ],
    };
    PayloadContract {
        open: false,
        direct: false,
        fields,
    }
}

/// Every `SopTriggerSource` except `Channel` becomes a bound source whose field
/// specs come from the source's own `TriggerBehavior`; every inbound-capable
/// `ChannelKind` becomes a channel row with whatever configured aliases match
/// it. No per-source field list lives here: the source describes its own fields.
#[must_use]
pub fn build_registry(configured: &[ConfiguredChannel]) -> TriggerSourceRegistry {
    use crate::sop::types::SopTriggerSource;

    let bound = SopTriggerSource::iter()
        .filter(|source| *source != SopTriggerSource::Channel)
        .map(|source| BoundTriggerSource {
            source: source.to_string(),
            fields: source.field_specs(),
            condition: condition_contract_for(source),
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
                condition: Some(channel_condition_fields(kind)),
            }
        })
        .collect();

    let sources = SopTriggerSource::iter().map(|s| s.to_string()).collect();

    TriggerSourceRegistry {
        sources,
        bound,
        channels,
        operators: crate::sop::condition::ConditionOp::catalog(),
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
        use crate::sop::types::{SopTriggerSource, sample_trigger};

        let registry = build_registry(&[]);
        for source in SopTriggerSource::iter() {
            let trigger = sample_trigger(source);
            let json = serde_json::to_value(&trigger).unwrap();
            let keys = json.as_object().unwrap();

            let Some(bound) = registry
                .bound
                .iter()
                .find(|b| b.source == source.to_string())
            else {
                // Not in `bound` means the source is config-derived: its options
                // come from live channel config, surfaced through the registry's
                // channel walk rather than a static field spec. Identify it
                // structurally (empty field_specs) and assert that channel walk
                // is populated, with no field names to match against serde.
                assert!(
                    source.field_specs().is_empty(),
                    "source {source} is absent from bound but still exposes static \
                     field specs; the registry dropped its bound entry"
                );
                assert!(
                    !registry.channels.is_empty(),
                    "config-derived source {source} has no channel walk to draw \
                     options from"
                );
                continue;
            };
            let spec_names: Vec<&str> = bound.fields.iter().map(|f| f.name.as_str()).collect();
            for field in &bound.fields {
                assert!(
                    keys.contains_key(&field.name),
                    "registry field '{}' for source {source} does not exist on the \
                     serialized trigger; field names drifted from SopTrigger serde fields",
                    field.name
                );
            }
            for key in keys.keys() {
                if key == "type" {
                    continue;
                }
                assert!(
                    spec_names.contains(&key.as_str()),
                    "serde field '{key}' on source {source} has no derived field spec; \
                     the TriggerFields derive dropped a variant field"
                );
            }
        }
    }

    #[test]
    fn derived_field_kinds_match_the_pinned_table() {
        // The TriggerFields derive infers each field's kind by convention
        // (name `condition` -> expression, `Vec<XxxKind>` -> options,
        // other `Vec<_>` -> list, else text). Those conventions are implicit,
        // so this table is the explicit contract: a new field that trips a
        // convention the wrong way fails here with a named source and field.
        use crate::sop::types::SopTriggerSource;

        let expected: &[(SopTriggerSource, &[(&str, TriggerFieldKind)])] = &[
            (
                SopTriggerSource::Mqtt,
                &[
                    ("topic", TriggerFieldKind::Text),
                    ("condition", TriggerFieldKind::Expression),
                ],
            ),
            (
                SopTriggerSource::Amqp,
                &[
                    ("routing_key", TriggerFieldKind::Text),
                    ("condition", TriggerFieldKind::Expression),
                ],
            ),
            (
                SopTriggerSource::Webhook,
                &[("path", TriggerFieldKind::Text)],
            ),
            (
                SopTriggerSource::Cron,
                &[("expression", TriggerFieldKind::Text)],
            ),
            (
                SopTriggerSource::Peripheral,
                &[
                    ("board", TriggerFieldKind::Text),
                    ("signal", TriggerFieldKind::Text),
                    ("condition", TriggerFieldKind::Expression),
                ],
            ),
            (
                SopTriggerSource::Filesystem,
                &[
                    ("path", TriggerFieldKind::Text),
                    ("events", TriggerFieldKind::List),
                    ("condition", TriggerFieldKind::Expression),
                ],
            ),
            (
                SopTriggerSource::Calendar,
                &[
                    ("calendar_source", TriggerFieldKind::Text),
                    ("calendar_ids", TriggerFieldKind::List),
                    ("condition", TriggerFieldKind::Expression),
                ],
            ),
            (SopTriggerSource::Manual, &[]),
        ];

        for (source, fields) in expected {
            let got = source.field_specs();
            let got_pairs: Vec<(&str, TriggerFieldKind)> =
                got.iter().map(|f| (f.name.as_str(), f.kind)).collect();
            let want_pairs: Vec<(&str, TriggerFieldKind)> = fields.to_vec();
            assert_eq!(
                got_pairs, want_pairs,
                "derived field kinds for source {source} drifted from the pinned \
                 table; a variant field tripped a derive convention",
            );
        }

        // Every source with derived fields must appear in the table, so adding
        // a source cannot skip the kind contract silently. Config-derived
        // sources (empty field_specs) carry no static kinds to pin and are
        // identified structurally, not by name.
        for source in SopTriggerSource::iter() {
            if source.field_specs().is_empty() && !expected.iter().any(|(s, _)| *s == source) {
                continue;
            }
            assert!(
                expected.iter().any(|(s, _)| *s == source),
                "trigger source {source} is missing from the field-kind table; add it"
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
    fn every_channel_surfaces_enumerated_walkable_fields() {
        // Anti-drift guard: no channel may fall back to a free-typed condition
        // path. Every inbound channel must enumerate a closed field set so the
        // authoring surface always renders a walkable picker.
        let registry = build_registry(&[]);
        for row in &registry.channels {
            let contract = row.condition.as_ref().unwrap_or_else(|| {
                panic!("channel {} must carry a condition contract", row.channel)
            });
            assert!(
                !contract.open,
                "channel {} must not use the open free-text contract",
                row.channel
            );
            assert!(
                !contract.fields.is_empty(),
                "channel {} must enumerate at least one condition field",
                row.channel
            );
        }
    }

    #[test]
    fn forge_condition_fields_cover_known_event_types() {
        let contract = channel_condition_fields(ChannelKind::Git);
        let event_type = contract
            .fields
            .iter()
            .find(|f| f.path == "event_type")
            .expect("forge contract must expose event_type");
        assert_eq!(event_type.value_type, ConditionValueType::Enum);
        assert_eq!(event_type.options, FORGE_EVENT_TYPES);
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

    #[test]
    fn condition_field_presence_matches_payload_contract() {
        // A source exposes a `condition` config field iff it carries a payload
        // contract. This is the anti-drift guard: add a `condition` field to a
        // trigger variant and you must give it a contract, and vice-versa.
        let registry = build_registry(&[]);
        for bound in &registry.bound {
            let has_condition_field = bound
                .fields
                .iter()
                .any(|f| f.kind == TriggerFieldKind::Expression);
            assert_eq!(
                has_condition_field,
                bound.condition.is_some(),
                "source {} has condition field={has_condition_field} but contract={}; \
                 the payload contract and the `condition` field must agree",
                bound.source,
                bound.condition.is_some()
            );
        }
    }

    #[test]
    fn known_shape_contracts_enumerate_fields() {
        let registry = build_registry(&[]);
        let fs = registry
            .bound
            .iter()
            .find(|b| b.source == "filesystem")
            .and_then(|b| b.condition.as_ref())
            .expect("filesystem must carry a payload contract");
        assert!(!fs.open, "filesystem payload is a known shape, not open");
        assert!(fs.fields.iter().any(|f| f.path == "event"));
        assert!(fs.fields.iter().any(|f| f.path == "path"));

        let peripheral = registry
            .bound
            .iter()
            .find(|b| b.source == "peripheral")
            .and_then(|b| b.condition.as_ref())
            .expect("peripheral must carry a payload contract");
        assert!(peripheral.direct, "peripheral payload is a bare scalar");

        let mqtt = registry
            .bound
            .iter()
            .find(|b| b.source == "mqtt")
            .and_then(|b| b.condition.as_ref())
            .expect("mqtt must carry a payload contract");
        assert!(mqtt.open, "mqtt payload is arbitrary user JSON");
    }

    #[test]
    fn registry_carries_operator_catalog() {
        let registry = build_registry(&[]);
        assert_eq!(
            registry.operators,
            crate::sop::condition::ConditionOp::catalog(),
            "registry must surface the operator catalog verbatim"
        );
    }
}
