use std::collections::BTreeMap;

use zeroclaw_config::traits::{PropFieldInfo, PropKind};
use zeroclaw_runtime::flow::{Node, NodeId, Outcome, Prompt, Spec, Step};
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

pub fn build_spec(
    fields: Vec<PropFieldInfo>,
    section_prefix: &str,
    layer: &str,
    instance: &str,
    success: Outcome,
) -> Option<Spec> {
    let mut all = section_fields(fields, section_prefix);
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
            prompt: Prompt::new(prompt_text(field), response_type_for(field)),
            on_success,
            on_failure: Step::Node(id.clone()),
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
}
