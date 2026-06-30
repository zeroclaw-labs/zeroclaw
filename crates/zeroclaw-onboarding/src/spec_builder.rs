use std::collections::BTreeMap;

use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::{PropFieldInfo, PropKind};
use zeroclaw_runtime::agent::personality::EDITABLE_PERSONALITY_FILES;
use zeroclaw_runtime::agent::personality_templates::{TemplateContext, render};
use zeroclaw_runtime::flow::{Localizable, Node, NodeId, Outcome, Prompt, Spec, Step, WriteTarget};
use zeroclaw_runtime::response_type::{ChoiceOption, ResponseType, ResponseValue};

const OPTION_PREFIX: &str = "Option<";

pub fn section_fields(fields: Vec<PropFieldInfo>, section_prefix: &str) -> Vec<PropFieldInfo> {
    let dotted = format!("{section_prefix}.");
    fields
        .into_iter()
        .filter(|field| field.name.starts_with(&dotted))
        .collect()
}

pub fn required_fields(fields: Vec<PropFieldInfo>, section_prefix: &str) -> Vec<PropFieldInfo> {
    section_fields(fields, section_prefix)
        .into_iter()
        .filter(|field| !field.type_hint.starts_with(OPTION_PREFIX))
        .collect()
}

pub fn response_type_for(field: &PropFieldInfo) -> ResponseType {
    if field.is_secret {
        return ResponseType::Secret;
    }
    match field.kind {
        PropKind::Bool => ResponseType::YesNo,
        PropKind::Enum => {
            let options: Vec<ChoiceOption> = field
                .enum_variants
                .map(|variants| variants())
                .unwrap_or_default()
                .into_iter()
                .map(|value| ChoiceOption {
                    label: value.clone(),
                    value,
                })
                .collect();
            if options.is_empty() {
                ResponseType::FreeformText
            } else {
                ResponseType::Choice { options }
            }
        }
        PropKind::Integer | PropKind::Float => ResponseType::Number,
        PropKind::String
        | PropKind::AliasRef
        | PropKind::StringArray
        | PropKind::ObjectArray
        | PropKind::Object => ResponseType::FreeformText,
    }
}

fn prompt_text(field: &PropFieldInfo) -> String {
    if field.description.is_empty() {
        field.name.clone()
    } else {
        field.description.to_string()
    }
}

fn write_prop_for(field: &PropFieldInfo, section_prefix: &str) -> String {
    if !field.is_secret {
        return field.name.clone();
    }
    let type_prefix = section_prefix
        .rsplit_once('.')
        .map(|(prefix, _alias)| prefix)
        .unwrap_or(section_prefix);
    let leaf = field
        .name
        .strip_prefix(&format!("{section_prefix}."))
        .unwrap_or(&field.name);
    format!("{type_prefix}.{leaf}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldScope {
    All,
    RequiredOnly,
}

pub fn build_spec(
    fields: Vec<PropFieldInfo>,
    section_prefix: &str,
    layer: &str,
    instance: &str,
    success: Outcome,
) -> Option<Spec> {
    build_spec_scoped(
        fields,
        section_prefix,
        layer,
        instance,
        success,
        FieldScope::All,
    )
}

pub fn build_spec_scoped(
    fields: Vec<PropFieldInfo>,
    section_prefix: &str,
    layer: &str,
    instance: &str,
    success: Outcome,
    scope: FieldScope,
) -> Option<Spec> {
    let mut all = match scope {
        FieldScope::All => section_fields(fields, section_prefix),
        FieldScope::RequiredOnly => required_fields(fields, section_prefix),
    };
    if all.is_empty() {
        return None;
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));

    let ids: Vec<NodeId> = all.iter().map(|field| NodeId::new(&field.name)).collect();

    let mut nodes = BTreeMap::new();
    for (index, field) in all.iter().enumerate() {
        let id = ids[index].clone();
        let on_success = match ids.get(index + 1) {
            Some(next) => Step::Node(next.clone()),
            None => Step::Terminal(success.clone()),
        };
        let optional = field.type_hint.starts_with(OPTION_PREFIX);
        let node = Node {
            id: id.clone(),
            layer: layer.to_string(),
            instance: instance.to_string(),
            prop: write_prop_for(field, section_prefix),
            optional,
            prompt: Prompt::new(prompt_text(field), response_type_for(field))
                .with_message(Localizable::new(&field.name).with_arg("text", prompt_text(field))),
            on_success,
            on_failure: Step::Node(id.clone()),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(validate_response),
        };
        nodes.insert(id, node);
    }

    Some(Spec {
        start: ids[0].clone(),
        nodes,
    })
}

fn validate_response(response: &ResponseValue) -> Result<(), ()> {
    match response {
        ResponseValue::Secret(secret) if secret.expose().is_empty() => Err(()),
        ResponseValue::FreeformText(text) if text.is_empty() => Err(()),
        ResponseValue::Number(number) if number.is_empty() => Err(()),
        ResponseValue::Choice(choice) if choice.is_empty() => Err(()),
        _ => Ok(()),
    }
}

const PG_DECISION: &str = "peer_group.decision";
const PG_BIND_NEW: &str = "peer_group.bind_new";
const PG_SKIP_VALUE: &str = "skip";
const PG_NEW_VALUE: &str = "new";
const PG_ATTACH_PREFIX: &str = "attach:";

/// Derive a deterministic peer-group name for the new-group branch from the
/// channel instance, so the group's prop paths are static at spec-build time
/// (the flow writes fixed prop paths; a free-text name would have no path).
fn derived_group_name(channel_type: &str, instance: &str) -> String {
    format!("{channel_type}_{instance}")
}

/// The dotted `<type>.<alias>` channel ref a peer group binds, derived from the
/// `channels.<type>.<alias>` section prefix by stripping the `channels.` head.
/// Matches `PeerGroupConfig.channel`'s `<type>.<alias>` form.
fn bound_channel_ref(section_prefix: &str) -> Option<String> {
    section_prefix.strip_prefix("channels.").map(str::to_string)
}

/// Splice a peer-group decision branch onto a freshly built channel spec. When a
/// channel is created the operator chooses, via a real `Choice` spec node, to
/// skip, attach the channel to an existing group, or create a new group and walk
/// its `PeerGroupConfig` fields. Existing groups come from `config.peer_groups`
/// (never hardcoded); the bound channel ref is written into the chosen group.
///
/// The base spec's terminal field repoints to the decision node; the decision
/// node's value-keyed `branches` route each option to its own next node. Returns
/// the spec unchanged when the section is not a channel section or yields no
/// bindable ref.
pub fn append_peer_group_branch(
    mut spec: Spec,
    section_prefix: &str,
    instance: &str,
    config: &Config,
    success: Outcome,
) -> Spec {
    let Some(channel_ref) = bound_channel_ref(section_prefix) else {
        return spec;
    };
    let channel_type = section_prefix
        .strip_prefix("channels.")
        .and_then(|rest| rest.split('.').next())
        .unwrap_or(section_prefix);

    let mut existing: Vec<String> = config.peer_groups.keys().cloned().collect();
    existing.sort();

    let decision_id = NodeId::new(PG_DECISION);

    let mut options = vec![
        ChoiceOption {
            value: PG_SKIP_VALUE.to_string(),
            label: "Skip peer-group binding".to_string(),
        },
        ChoiceOption {
            value: PG_NEW_VALUE.to_string(),
            label: "Create a new peer group".to_string(),
        },
    ];
    options.extend(existing.iter().map(|name| ChoiceOption {
        value: format!("{PG_ATTACH_PREFIX}{name}"),
        label: format!("Attach to existing group '{name}'"),
    }));

    repoint_terminal_to(&mut spec, &decision_id);

    let new_group = derived_group_name(channel_type, instance);
    let new_group_chain =
        new_group_field_chain(&new_group, instance, &channel_ref, config, success.clone());
    let new_entry = new_group_chain
        .as_ref()
        .map(|(start, _)| start.clone())
        .unwrap_or_else(|| NodeId::new(PG_BIND_NEW));

    let mut branches: Vec<(ResponseValue, Step)> = vec![
        (
            ResponseValue::Choice(PG_SKIP_VALUE.to_string()),
            Step::Terminal(success.clone()),
        ),
        (
            ResponseValue::Choice(PG_NEW_VALUE.to_string()),
            Step::Node(new_entry),
        ),
    ];
    for name in &existing {
        let attach_id = NodeId::new(format!("peer_group.attach.{name}"));
        branches.push((
            ResponseValue::Choice(format!("{PG_ATTACH_PREFIX}{name}")),
            Step::Node(attach_id.clone()),
        ));
        spec.nodes.insert(
            attach_id.clone(),
            bind_channel_node(attach_id, name, &channel_ref, instance, success.clone()),
        );
    }

    let decision = Node {
        id: decision_id.clone(),
        layer: "channel".to_string(),
        instance: instance.to_string(),
        prop: String::new(),
        optional: false,
        prompt: Prompt::new(
            "Bind this channel into a peer group?",
            ResponseType::Choice { options },
        )
        .with_message(
            Localizable::new(PG_DECISION).with_arg("text", "Bind this channel into a peer group?"),
        ),
        on_success: Step::Terminal(success.clone()),
        on_failure: Step::Node(decision_id.clone()),
        branches,
        ensure_map_key: None,
        write_target: None,
        validate: Box::new(validate_response),
    };
    spec.nodes.insert(decision_id, decision);

    if let Some((_, chain)) = new_group_chain {
        for node in chain {
            spec.nodes.insert(node.id.clone(), node);
        }
    }

    spec
}

/// Write the channel ref into an existing group's `channel` prop, then terminate.
fn bind_channel_node(
    id: NodeId,
    group: &str,
    channel_ref: &str,
    instance: &str,
    success: Outcome,
) -> Node {
    let prop = format!("peer_groups.{group}.channel");
    let preset = channel_ref.to_string();
    Node {
        id,
        layer: "channel".to_string(),
        instance: instance.to_string(),
        prop,
        optional: false,
        prompt: Prompt::new(
            format!("Confirm binding this channel into '{group}'?"),
            ResponseType::Choice {
                options: vec![ChoiceOption {
                    value: preset.clone(),
                    label: format!("Bind {preset} into {group}"),
                }],
            },
        )
        .with_message(
            Localizable::new("peer_group.bind")
                .with_arg("group", group)
                .with_arg("channel", &preset),
        ),
        on_success: Step::Terminal(success),
        on_failure: Step::Terminal(Outcome::Cancelled),
        branches: Vec::new(),
        ensure_map_key: None,
        write_target: None,
        validate: Box::new(validate_response),
    }
}

/// Build the new-group field walk: create the derived group key so its props are
/// enumerable, auto-bind the channel ref as the first node, then walk the
/// remaining `PeerGroupConfig` fields. Returns the entry node id and the chain.
fn new_group_field_chain(
    group: &str,
    instance: &str,
    channel_ref: &str,
    config: &Config,
    success: Outcome,
) -> Option<(NodeId, Vec<Node>)> {
    let mut probe = config.clone();
    if probe.create_map_key("peer_groups", group).is_err() {
        return None;
    }
    let prefix = format!("peer_groups.{group}");
    let mut fields = section_fields(probe.prop_fields(), &prefix);
    fields.retain(|field| field.name != format!("{prefix}.channel"));
    fields.sort_by(|a, b| a.name.cmp(&b.name));

    let bind_id = NodeId::new(PG_BIND_NEW);
    let field_ids: Vec<NodeId> = fields.iter().map(|f| NodeId::new(&f.name)).collect();

    let mut chain = Vec::new();

    let bind_next = field_ids
        .first()
        .cloned()
        .map(Step::Node)
        .unwrap_or_else(|| Step::Terminal(success.clone()));

    chain.push(Node {
        id: bind_id.clone(),
        layer: "channel".to_string(),
        instance: instance.to_string(),
        prop: format!("{prefix}.channel"),
        optional: false,
        prompt: Prompt::new(
            format!("Bind this channel into the new group '{group}'?"),
            ResponseType::Choice {
                options: vec![ChoiceOption {
                    value: channel_ref.to_string(),
                    label: format!("Create '{group}' bound to {channel_ref}"),
                }],
            },
        )
        .with_message(
            Localizable::new("peer_group.new")
                .with_arg("group", group)
                .with_arg("channel", channel_ref),
        ),
        on_success: bind_next,
        on_failure: Step::Terminal(Outcome::Cancelled),
        branches: Vec::new(),
        ensure_map_key: Some(("peer_groups".to_string(), group.to_string())),
        write_target: None,
        validate: Box::new(validate_response),
    });

    for (index, field) in fields.iter().enumerate() {
        let id = field_ids[index].clone();
        let on_success = match field_ids.get(index + 1) {
            Some(next) => Step::Node(next.clone()),
            None => Step::Terminal(success.clone()),
        };
        let optional = field.type_hint.starts_with(OPTION_PREFIX);
        chain.push(Node {
            id: id.clone(),
            layer: "channel".to_string(),
            instance: instance.to_string(),
            prop: write_prop_for(field, &prefix),
            optional,
            prompt: Prompt::new(prompt_text(field), response_type_for(field))
                .with_message(Localizable::new(&field.name).with_arg("text", prompt_text(field))),
            on_success,
            on_failure: Step::Node(id.clone()),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(validate_response),
        });
    }

    Some((bind_id, chain))
}

/// Repoint every node whose `on_success` is a terminal into the given node, so
/// the channel field chain flows into the peer-group decision instead of ending.
fn repoint_terminal_to(spec: &mut Spec, target: &NodeId) {
    for node in spec.nodes.values_mut() {
        if matches!(node.on_success, Step::Terminal(_)) {
            node.on_success = Step::Node(target.clone());
        }
    }
}

const PERSONALITY_DECISION_PREFIX: &str = "personality.decision";
const PERSONALITY_AUTHOR_NODE_PREFIX: &str = "personality.author";
const PERSONALITY_TEMPLATE_NODE_PREFIX: &str = "personality.template";
const PERSONALITY_AUTHOR_VALUE: &str = "author";
const PERSONALITY_TEMPLATE_VALUE: &str = "template";
const PERSONALITY_SKIP_VALUE: &str = "skip";

/// Splice a per-file personality decision chain onto a spec. Each editable
/// personality file gets a real `Choice` decision node (author / use-template /
/// skip) whose branches route to a freeform author node writing the response
/// into the agent workspace, a literal node writing the pre-rendered template,
/// or straight on to the next file. The file list is the canonical registry,
/// never hardcoded; a file with no template offers only author / skip. The base
/// spec's terminal edges repoint into the first file's decision.
pub fn append_personality_branch(
    mut spec: Spec,
    agent_alias: &str,
    ctx: &TemplateContext,
    success: Outcome,
) -> Spec {
    let base_ids: Vec<NodeId> = spec.nodes.keys().cloned().collect();
    let files: Vec<&'static str> = EDITABLE_PERSONALITY_FILES.to_vec();
    let mut next_step = Step::Terminal(success);

    for filename in files.iter().rev() {
        let decision_id = NodeId::new(format!("{PERSONALITY_DECISION_PREFIX}.{filename}"));
        let rendered = render(filename, ctx);

        let mut options = vec![ChoiceOption {
            value: PERSONALITY_AUTHOR_VALUE.to_string(),
            label: format!("Author {filename}"),
        }];
        if rendered.is_some() {
            options.push(ChoiceOption {
                value: PERSONALITY_TEMPLATE_VALUE.to_string(),
                label: format!("Use the {filename} template"),
            });
        }
        options.push(ChoiceOption {
            value: PERSONALITY_SKIP_VALUE.to_string(),
            label: format!("Skip {filename}"),
        });

        let author_id = NodeId::new(format!("{PERSONALITY_AUTHOR_NODE_PREFIX}.{filename}"));
        spec.nodes.insert(
            author_id.clone(),
            personality_author_node(author_id.clone(), agent_alias, filename, next_step.clone()),
        );

        let mut branches: Vec<(ResponseValue, Step)> = vec![
            (
                ResponseValue::Choice(PERSONALITY_AUTHOR_VALUE.to_string()),
                Step::Node(author_id),
            ),
            (
                ResponseValue::Choice(PERSONALITY_SKIP_VALUE.to_string()),
                next_step.clone(),
            ),
        ];

        if let Some(content) = rendered {
            let template_id = NodeId::new(format!("{PERSONALITY_TEMPLATE_NODE_PREFIX}.{filename}"));
            spec.nodes.insert(
                template_id.clone(),
                personality_template_node(
                    template_id.clone(),
                    agent_alias,
                    filename,
                    content,
                    next_step.clone(),
                ),
            );
            branches.push((
                ResponseValue::Choice(PERSONALITY_TEMPLATE_VALUE.to_string()),
                Step::Node(template_id),
            ));
        }

        let decision = Node {
            id: decision_id.clone(),
            layer: "agent".to_string(),
            instance: agent_alias.to_string(),
            prop: String::new(),
            optional: false,
            prompt: Prompt::new(
                format!("Set up {filename}?"),
                ResponseType::Choice { options },
            ),
            on_success: next_step.clone(),
            on_failure: Step::Node(decision_id.clone()),
            branches,
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(validate_response),
        };
        spec.nodes.insert(decision_id.clone(), decision);
        next_step = Step::Node(decision_id);
    }

    if let Step::Node(first) = &next_step {
        let first = first.clone();
        repoint_personality_entry(&mut spec, &base_ids, &first);
        spec.start = first;
    }

    spec
}

fn repoint_personality_entry(spec: &mut Spec, base_ids: &[NodeId], target: &NodeId) {
    for id in base_ids {
        if let Some(node) = spec.nodes.get_mut(id)
            && matches!(&node.on_success, Step::Terminal(_))
        {
            node.on_success = Step::Node(target.clone());
        }
    }
}

fn personality_author_node(id: NodeId, agent_alias: &str, filename: &str, next: Step) -> Node {
    Node {
        id: id.clone(),
        layer: "agent".to_string(),
        instance: agent_alias.to_string(),
        prop: String::new(),
        optional: false,
        prompt: Prompt::new(
            format!("Write the contents of {filename}"),
            ResponseType::FreeformText,
        ),
        on_success: next,
        on_failure: Step::Node(id),
        branches: Vec::new(),
        ensure_map_key: None,
        write_target: Some(WriteTarget::WorkspaceFileFromResponse {
            agent_alias: agent_alias.to_string(),
            filename: filename.to_string(),
        }),
        validate: Box::new(validate_response),
    }
}

fn personality_template_node(
    id: NodeId,
    agent_alias: &str,
    filename: &str,
    content: String,
    next: Step,
) -> Node {
    Node {
        id: id.clone(),
        layer: "agent".to_string(),
        instance: agent_alias.to_string(),
        prop: String::new(),
        optional: false,
        prompt: Prompt::new(
            format!("Write the {filename} template to the workspace?"),
            ResponseType::YesNo,
        ),
        on_success: next,
        on_failure: Step::Node(id),
        branches: Vec::new(),
        ensure_map_key: None,
        write_target: Some(WriteTarget::WorkspaceFileLiteral {
            agent_alias: agent_alias.to_string(),
            filename: filename.to_string(),
            content,
        }),
        validate: Box::new(validate_response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{Config, MatrixConfig};

    fn matrix_config() -> Config {
        let mut config = Config::default();
        config
            .channels
            .matrix
            .insert("home".to_string(), MatrixConfig::default());
        config
    }

    fn matrix_fields() -> Vec<PropFieldInfo> {
        matrix_config().prop_fields()
    }

    #[test]
    fn required_fields_excludes_option_typed() {
        let required = required_fields(matrix_fields(), "channels.matrix.home");
        for field in &required {
            assert!(
                !field.type_hint.starts_with("Option<"),
                "{} is Option-typed and should not be required",
                field.name
            );
        }
        assert!(
            required
                .iter()
                .any(|field| field.name == "channels.matrix.home.homeserver"),
            "homeserver is a non-Option String and must be required"
        );
        assert!(
            !required
                .iter()
                .any(|field| field.name == "channels.matrix.home.access_token"),
            "access_token is Option<String> and is not required by type"
        );
    }

    #[test]
    fn bool_field_maps_to_yes_no() {
        let required = required_fields(matrix_fields(), "channels.matrix.home");
        let mention = required
            .iter()
            .find(|field| field.name == "channels.matrix.home.mention_only")
            .expect("mention_only is a non-Option bool");
        assert_eq!(response_type_for(mention), ResponseType::YesNo);
    }

    #[test]
    fn enum_field_offers_registry_variants_as_choices() {
        let required = required_fields(matrix_fields(), "channels.matrix.home");
        let stream = required
            .iter()
            .find(|field| field.name == "channels.matrix.home.stream_mode")
            .expect("stream_mode is a non-Option enum");
        assert_eq!(stream.kind, PropKind::Enum);
        let ResponseType::Choice { options } = response_type_for(stream) else {
            panic!("enum field must map to a Choice of its registry variants");
        };
        let values: Vec<String> = options.into_iter().map(|option| option.value).collect();
        assert_eq!(values, vec!["off", "partial", "multi_message"]);
    }

    #[test]
    fn numeric_field_maps_to_number() {
        let required = required_fields(matrix_fields(), "channels.matrix.home");
        let interval = required
            .iter()
            .find(|field| field.name == "channels.matrix.home.draft_update_interval_ms")
            .expect("draft_update_interval_ms is a non-Option u64");
        assert!(matches!(interval.kind, PropKind::Integer | PropKind::Float));
        assert_eq!(response_type_for(interval), ResponseType::Number);
    }

    #[test]
    fn build_spec_starts_at_first_required_field() {
        let spec = build_spec(
            matrix_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Cancelled,
        )
        .expect("matrix has required fields");
        assert!(spec.nodes.contains_key(&spec.start));
    }

    #[test]
    fn required_only_scope_drops_option_typed_nodes() {
        let all = build_spec_scoped(
            matrix_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Cancelled,
            FieldScope::All,
        )
        .expect("all scope yields a spec");
        let required = build_spec_scoped(
            matrix_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Cancelled,
            FieldScope::RequiredOnly,
        )
        .expect("required scope yields a spec");
        assert!(
            required.nodes.len() < all.nodes.len(),
            "required-only must ask fewer fields than the full walk"
        );
        assert!(
            required
                .nodes
                .contains_key(&NodeId::new("channels.matrix.home.homeserver")),
            "a non-Option String stays in the required walk"
        );
        assert!(
            !required
                .nodes
                .contains_key(&NodeId::new("channels.matrix.home.access_token")),
            "an Option<String> is dropped from the required walk"
        );
    }

    #[test]
    fn each_field_prompt_carries_a_localizable_keyed_by_field_name() {
        let spec = build_spec(
            matrix_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Cancelled,
        )
        .expect("matrix has fields");
        for (id, node) in &spec.nodes {
            let descriptor = node.prompt.message.as_ref().unwrap_or_else(|| {
                panic!("field prompt for {id:?} must carry a localizable descriptor")
            });
            assert_eq!(
                descriptor.message_id, id.0,
                "every node's descriptor id is its own field name, not a cherry-picked one"
            );
            assert!(
                descriptor
                    .args
                    .iter()
                    .any(|(name, value)| name == "text" && !value.is_empty()),
                "node {id:?} must carry its schema-sourced prompt text as a data arg"
            );
        }
    }

    use async_trait::async_trait;
    use zeroclaw_runtime::flow::{FlowTransport, TransportResult};
    use zeroclaw_runtime::response_type::SecretValue;

    struct SteeredTransport {
        decision: String,
        emitted: Vec<Outcome>,
        asks: Vec<String>,
    }

    impl SteeredTransport {
        fn new(decision: &str) -> Self {
            Self {
                decision: decision.to_string(),
                emitted: Vec::new(),
                asks: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl FlowTransport for SteeredTransport {
        async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
            self.asks.push(prompt.text.clone());
            if prompt.text.contains("Bind this channel into a peer group?") {
                return Ok(ResponseValue::Choice(self.decision.clone()));
            }
            Ok(match &prompt.response_type {
                ResponseType::Secret => ResponseValue::Secret(SecretValue::new("tok".into())),
                ResponseType::YesNo => ResponseValue::YesNo(true),
                ResponseType::Number => ResponseValue::Number("5".into()),
                ResponseType::Choice { options } => ResponseValue::Choice(options[0].value.clone()),
                ResponseType::FreeformText => ResponseValue::FreeformText("x".into()),
            })
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    fn channel_spec_with_pg(config: &Config) -> Spec {
        let base = build_spec(
            config.prop_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Completed { configured: vec![] },
        )
        .expect("matrix base spec");
        append_peer_group_branch(
            base,
            "channels.matrix.home",
            "home",
            config,
            Outcome::Completed { configured: vec![] },
        )
    }

    #[tokio::test]
    async fn skip_branch_reaches_success_without_touching_groups() {
        let mut config = matrix_config();
        let spec = channel_spec_with_pg(&config);
        let mut transport = SteeredTransport::new(PG_SKIP_VALUE);
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
        assert!(
            config.peer_groups.is_empty(),
            "skip must not create or mutate any peer group"
        );
        assert!(
            transport
                .asks
                .iter()
                .any(|text| text.contains("Bind this channel into a peer group?")),
            "the decision node must be a real walked spec node"
        );
    }

    #[tokio::test]
    async fn new_branch_creates_group_binds_ref_and_walks_fields() {
        let mut config = matrix_config();
        let spec = channel_spec_with_pg(&config);
        let mut transport = SteeredTransport::new(PG_NEW_VALUE);
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
        let group = config
            .peer_groups
            .get("matrix_home")
            .expect("new-group branch must create the derived group");
        assert_eq!(
            group.channel.as_str(),
            "matrix.home",
            "the bound channel ref is written into the new group"
        );
    }

    #[tokio::test]
    async fn attach_branch_binds_ref_into_chosen_existing_group() {
        let mut config = matrix_config();
        config
            .create_map_key("peer_groups", "ops")
            .expect("seed an existing group");
        let spec = channel_spec_with_pg(&config);
        let mut transport = SteeredTransport::new("attach:ops");
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
        assert_eq!(
            config.peer_groups.get("ops").unwrap().channel.as_str(),
            "matrix.home",
            "attach writes the bound ref into the chosen existing group"
        );
    }

    #[test]
    fn existing_groups_drive_attach_options_not_hardcoded() {
        let mut config = matrix_config();
        config.create_map_key("peer_groups", "alpha").unwrap();
        config.create_map_key("peer_groups", "beta").unwrap();
        let spec = channel_spec_with_pg(&config);
        let decision = spec
            .nodes
            .get(&NodeId::new(PG_DECISION))
            .expect("decision node present");
        let ResponseType::Choice { options } = &decision.prompt.response_type else {
            panic!("decision node must be a Choice");
        };
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert!(values.contains(&"attach:alpha"));
        assert!(values.contains(&"attach:beta"));
        assert!(values.contains(&PG_SKIP_VALUE));
        assert!(values.contains(&PG_NEW_VALUE));
    }

    struct PersonalitySteeredTransport {
        decision: String,
        authored: String,
        emitted: Vec<Outcome>,
        asks: Vec<String>,
    }

    impl PersonalitySteeredTransport {
        fn new(decision: &str, authored: &str) -> Self {
            Self {
                decision: decision.to_string(),
                authored: authored.to_string(),
                emitted: Vec::new(),
                asks: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl FlowTransport for PersonalitySteeredTransport {
        async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
            self.asks.push(prompt.text.clone());
            Ok(match &prompt.response_type {
                ResponseType::Choice { options } => {
                    let chosen = options
                        .iter()
                        .find(|option| option.value == self.decision)
                        .map(|option| option.value.clone())
                        .unwrap_or_else(|| options[0].value.clone());
                    ResponseValue::Choice(chosen)
                }
                ResponseType::FreeformText => ResponseValue::FreeformText(self.authored.clone()),
                ResponseType::YesNo => ResponseValue::YesNo(true),
                ResponseType::Number => ResponseValue::Number("1".into()),
                ResponseType::Secret => ResponseValue::Secret(SecretValue::new("tok".into())),
            })
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    fn agent_config() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().to_path_buf(),
            ..Default::default()
        };
        (tmp, config)
    }

    fn personality_spec(_config: &Config) -> Spec {
        let base = Spec {
            start: NodeId::new(PERSONALITY_DECISION_PREFIX),
            nodes: BTreeMap::new(),
        };
        append_personality_branch(
            base,
            "scout",
            &TemplateContext::default(),
            Outcome::Completed { configured: vec![] },
        )
    }

    #[tokio::test]
    async fn skip_all_personality_files_writes_nothing_and_completes() {
        let (_tmp, mut config) = agent_config();
        let spec = personality_spec(&config);
        let mut transport = PersonalitySteeredTransport::new(PERSONALITY_SKIP_VALUE, "");
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
        let workspace = config.agent_workspace_dir("scout");
        for filename in EDITABLE_PERSONALITY_FILES {
            assert!(
                !workspace.join(filename).exists(),
                "skip must not write {filename}"
            );
        }
    }

    #[tokio::test]
    async fn author_branch_writes_response_text_to_each_file() {
        let (_tmp, mut config) = agent_config();
        let spec = personality_spec(&config);
        let mut transport =
            PersonalitySteeredTransport::new(PERSONALITY_AUTHOR_VALUE, "hand authored");
        spec.walk(&mut transport, &mut config).await.unwrap();
        let workspace = config.agent_workspace_dir("scout");
        let written = std::fs::read_to_string(workspace.join("SOUL.md")).unwrap();
        assert_eq!(written, "hand authored");
    }

    #[tokio::test]
    async fn template_branch_writes_rendered_template_to_each_file() {
        let (_tmp, mut config) = agent_config();
        let spec = personality_spec(&config);
        let mut transport = PersonalitySteeredTransport::new(PERSONALITY_TEMPLATE_VALUE, "");
        spec.walk(&mut transport, &mut config).await.unwrap();
        let workspace = config.agent_workspace_dir("scout");
        let rendered = zeroclaw_runtime::agent::personality_templates::render(
            "SOUL.md",
            &TemplateContext::default(),
        )
        .expect("SOUL.md renders");
        let written = std::fs::read_to_string(workspace.join("SOUL.md")).unwrap();
        assert_eq!(written, rendered);
    }

    #[test]
    fn every_editable_file_gets_a_decision_node_from_the_registry() {
        let (_tmp, config) = agent_config();
        let spec = personality_spec(&config);
        for filename in EDITABLE_PERSONALITY_FILES {
            let decision_id = NodeId::new(format!("{PERSONALITY_DECISION_PREFIX}.{filename}"));
            let node = spec
                .nodes
                .get(&decision_id)
                .unwrap_or_else(|| panic!("missing decision node for {filename}"));
            let ResponseType::Choice { options } = &node.prompt.response_type else {
                panic!("personality decision for {filename} must be a Choice");
            };
            let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
            assert!(values.contains(&PERSONALITY_AUTHOR_VALUE));
            assert!(values.contains(&PERSONALITY_TEMPLATE_VALUE));
            assert!(values.contains(&PERSONALITY_SKIP_VALUE));
        }
    }
}
