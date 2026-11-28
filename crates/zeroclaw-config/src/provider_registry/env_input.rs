//! Environment input for exact registry keys. JSON pointers are internal
//! override identities; `__` remains the external segment delimiter.
use crate::schema::Config;
use anyhow::{Result, ensure};
use serde_json::Value;

pub(crate) fn path(config: &Config, tail: &str) -> Option<String> {
    let parts: Vec<_> = tail.split("__").collect();
    let ["providers", section, key, rest @ ..] = parts.as_slice() else {
        return None;
    };
    let mut segments = vec![
        "providers".to_owned(),
        (*section).to_owned(),
        (*key).to_owned(),
    ];
    match *section {
        "connections" | "model_presets" | "fallback" => {
            segments.extend(rest.iter().map(|part| (*part).to_owned()))
        }
        "models"
            if config
                .providers
                .models
                .canonical_model(&(*key).into())
                .is_some() =>
        {
            segments.extend(rest.iter().map(|part| (*part).to_owned()));
        }
        "models" => {
            segments = crate::migration::provider_inputs::legacy_env_segments(config, &parts)?;
        }
        _ => return None,
    }
    Some(format!(
        "/{}",
        segments
            .iter()
            .map(|part| part.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/")
    ))
}

pub(crate) fn snapshot(config: &Config, path: &str) -> Result<String> {
    snapshot_document(&serde_json::to_value(&config.providers)?, path)
}

pub(crate) fn snapshot_document(document: &Value, path: &str) -> Result<String> {
    let pointer = path
        .strip_prefix("/providers")
        .ok_or_else(|| anyhow::Error::msg("invalid registry override path"))?;
    let mut missing_parents = Vec::new();
    let mut parent = pointer.rsplit_once('/').map(|(parent, _)| parent);
    while let Some(path) = parent {
        if document.pointer(path).is_none_or(Value::is_null) {
            missing_parents.push(path.to_owned());
        }
        parent = path.rsplit_once('/').map(|(parent, _)| parent);
    }
    Ok(serde_json::to_string(&serde_json::json!({
        "value": document.pointer(pointer), "missing_parents": missing_parents,
    }))?)
}

/// Retire old provider override identities together with their source tables.
/// Compare normalized views of the admitted input and its pre-env input, so
/// field relocation uses the same migration as construction (no parallel map).
pub(crate) fn remap_after_promotion(source: &Config, candidate: &mut Config) -> Result<()> {
    fn retired_input(path: &str) -> bool {
        path.starts_with("providers.")
            || path.starts_with("decision_models.")
            || path.starts_with("memory.embedding_")
            || path.starts_with("embedding_routes.")
            || matches!(
                path,
                "transcription.api_key"
                    | "transcription.api_url"
                    | "transcription.model"
                    | "transcription.language"
                    | "transcription.initial_prompt"
            )
            || [
                "openai",
                "deepgram",
                "assemblyai",
                "google",
                "local_whisper",
            ]
            .iter()
            .any(|family| path.starts_with(&format!("transcription.{family}.")))
    }
    if !source
        .pre_override_snapshots
        .keys()
        .any(|path| retired_input(path))
    {
        return Ok(());
    }
    let mut clean = source.clone();
    crate::env_overrides::mask_env_overrides_for_save(&mut clean, &source.pre_override_snapshots)?;
    crate::migration::provider_inputs::normalize_definitions(&mut clean)?;
    let original = serde_json::to_value(clean.provider_registry()?)?;
    let admitted = serde_json::to_value(candidate.provider_registry()?)?;
    fn differences(original: &Value, admitted: &Value, path: &str, out: &mut Vec<String>) {
        if original == admitted {
            return;
        }
        if let (Some(left), Some(right)) = (original.as_object(), admitted.as_object()) {
            let keys: std::collections::BTreeSet<_> = left.keys().chain(right.keys()).collect();
            for key in keys {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                differences(
                    left.get(key).unwrap_or(&Value::Null),
                    right.get(key).unwrap_or(&Value::Null),
                    &path,
                    out,
                );
            }
        } else {
            out.push(path.to_owned());
        }
    }
    let mut paths = Vec::new();
    differences(&original, &admitted, "/providers", &mut paths);
    candidate
        .pre_override_snapshots
        .retain(|path, _| !retired_input(path));
    candidate
        .env_overridden_paths
        .retain(|path| !retired_input(path));
    // Migration may also change the consumer reference. Restore that reference
    // alongside env-created model/connection entries when saving.
    for (path, original, admitted) in [
        (
            "memory.embedding_model_ref",
            serde_json::to_value(&clean.memory.embedding_model_ref)?,
            serde_json::to_value(&candidate.memory.embedding_model_ref)?,
        ),
        (
            "memory.embedding_dimensions",
            serde_json::to_value(clean.memory.embedding_dimensions)?,
            serde_json::to_value(candidate.memory.embedding_dimensions)?,
        ),
        (
            "embedding_routes",
            serde_json::to_value(&clean.embedding_routes)?,
            serde_json::to_value(&candidate.embedding_routes)?,
        ),
    ] {
        if original != admitted {
            let saved = if original.is_null() {
                String::new()
            } else if let Some(value) = original.as_str() {
                value.to_owned()
            } else {
                original.to_string()
            };
            candidate
                .pre_override_snapshots
                .entry(path.into())
                .or_insert(saved);
            candidate.env_overridden_paths.insert(path.into());
        }
    }

    for path in paths {
        let saved = snapshot_document(&original, &path)?;
        candidate
            .pre_override_snapshots
            .entry(path.clone())
            .or_insert(saved);
        candidate.env_overridden_paths.insert(path);
    }
    Ok(())
}

pub(crate) fn set(config: &mut Config, path: &str, value: &str, restore: bool) -> Result<()> {
    if restore {
        let mut document = serde_json::to_value(&config.providers)?;
        restore_document(&mut document, path, value)?;
        config.providers = serde_json::from_value(document)?;
        return Ok(());
    }
    set_many(config, &[(path.to_owned(), value.to_owned())])
}

fn env_value(value: &str) -> (Value, Value) {
    let literal = if value.is_empty() {
        Value::Null
    } else {
        Value::String(value.to_owned())
    };
    let parsed = serde_json::from_str::<Value>(value)
        .ok()
        .filter(|value| !value.is_null() && !value.is_string())
        .unwrap_or_else(|| literal.clone());
    (parsed, literal)
}

/// An untyped transaction view lets historical whole-auth edits compose with
/// earlier edits before the final registry is admitted.
pub(crate) fn stage(document: &mut Value, path: &str, value: &str) -> Result<()> {
    let (parsed, literal) = env_value(value);
    let value = typed_env_value(document, path, &parsed, &literal)?.unwrap_or({
        // An incomplete auth variant cannot reveal a leaf's type yet. Keep
        // scalar input literal so later whole-auth expansion cannot copy a
        // numeric credential. Final batch admission resolves the actual type.
        if parsed.is_number() || parsed.is_boolean() {
            literal
        } else {
            parsed
        }
    });
    set_document(document, path, value, None)
}

fn typed_env_value(
    template: &Value,
    path: &str,
    parsed: &Value,
    literal: &Value,
) -> Result<Option<Value>> {
    // The schema is the source for the leaf's type, including credentials
    // that happen to look like JSON numbers. Probe independently so a valid
    // numeric option cannot turn into a string while repairing another leaf.
    let mut probe = template.clone();
    set_document(&mut probe, path, parsed.clone(), None)?;
    if serde_json::from_value::<crate::providers::Providers>(probe).is_ok() {
        return Ok(Some(parsed.clone()));
    }
    if parsed == literal {
        return Ok(None);
    }
    let mut probe = template.clone();
    set_document(&mut probe, path, literal.clone(), None)?;
    if serde_json::from_value::<crate::providers::Providers>(probe).is_ok() {
        Ok(Some(literal.clone()))
    } else {
        Ok(None)
    }
}

/// Stage all registry environment edits before decoding the final registry.
/// Compound auth overrides must not admit an incomplete intermediate variant.
pub(crate) fn set_many(config: &mut Config, edits: &[(String, String)]) -> Result<()> {
    let mut document = serde_json::to_value(&config.providers)?;
    let mut probe = document.clone();
    let parsed_edits: Vec<_> = edits
        .iter()
        .map(|(path, value)| (path, env_value(value)))
        .collect();
    let mut selected = vec![None; edits.len()];
    // Keep the probe schema-valid. Defer edits that require another edit
    // first (for example an OAuth leaf before its mode, or a mode change
    // before a whole-auth replacement removes the old API key). Each round
    // selects at least one additional edit or terminates; unrelated malformed
    // intermediate values never determine a different leaf's representation.
    loop {
        let mut progressed = false;
        for (index, (path, (parsed, literal))) in parsed_edits.iter().enumerate() {
            if selected[index].is_some() {
                continue;
            }
            if let Some(value) = typed_env_value(&probe, path, parsed, literal)? {
                set_document(&mut probe, path, value.clone(), None)?;
                selected[index] = Some(value);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    // Probe order only selects representations. Replaying the original order
    // preserves last-write semantics for overlapping leaf/whole-object edits.
    // An unresolved edit may be superseded by a later replacement; otherwise
    // final typed admission below reports the invalid completed configuration.
    for ((path, (parsed, _)), value) in parsed_edits.into_iter().zip(selected) {
        let value = value.unwrap_or(parsed);
        set_document(&mut document, path, value, None)?;
    }
    for (path, _) in edits {
        validate_option_override(&document, path)?;
    }
    config.providers = serde_json::from_value(document)?;
    Ok(())
}

fn validate_option_override(document: &Value, path: &str) -> Result<()> {
    let parts: Vec<_> = path
        .trim_start_matches('/')
        .split('/')
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect();
    let options = match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [
            "providers",
            section @ ("models" | "model_presets"),
            key,
            "options",
            ..,
        ] => document[*section][*key]["capability"]
            .as_str()
            .map(|capability| (capability, &document[*section][*key]["options"])),
        [
            "providers",
            "connections",
            key,
            "api",
            capability,
            "options",
            ..,
        ] => Some((
            *capability,
            &document["connections"][*key]["api"][*capability]["options"],
        )),
        _ => None,
    };
    if let Some((capability, options)) = options {
        for field in options.as_object().into_iter().flat_map(|map| map.keys()) {
            ensure!(
                super::option_fields(capability).contains(&field.as_str()),
                "unknown registry override field"
            );
        }
    }
    Ok(())
}

pub(crate) fn restore(
    config: &mut Config,
    snapshots: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let mut document = serde_json::to_value(&config.providers)?;
    for (path, value) in snapshots
        .iter()
        .filter(|(path, _)| path.starts_with("/providers/"))
    {
        restore_document(&mut document, path, value)?;
    }
    config.providers = serde_json::from_value(document)?;
    Ok(())
}

fn restore_document(document: &mut Value, path: &str, value: &str) -> Result<()> {
    let saved: Value = serde_json::from_str(value)?;
    set_document(document, path, saved["value"].clone(), Some(&saved))
}

fn set_document(
    document: &mut Value,
    path: &str,
    value: Value,
    saved: Option<&Value>,
) -> Result<()> {
    let pointer = path
        .strip_prefix("/providers/")
        .ok_or_else(|| anyhow::Error::msg("invalid registry override path"))?;
    let parts: Vec<String> = pointer
        .split('/')
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect();
    let mut node = &mut *document;
    if parts.len() == 1 {
        // Promotion can create an entire previously absent collection. Only
        // snapshot restoration may remove it; env edits still target leaves.
        ensure!(saved.is_some(), "override must target a registry leaf");
        let map = node
            .as_object_mut()
            .ok_or_else(|| anyhow::Error::msg("registry must be a table"))?;
        if value.is_null() {
            map.remove(&parts[0]);
        } else {
            map.insert(parts[0].clone(), value);
        }
        return Ok(());
    }
    for (index, part) in parts[..parts.len() - 1].iter().enumerate() {
        if index == 0 && matches!(part.as_str(), "fallback" | "model_presets") {
            node = node
                .as_object_mut()
                .ok_or_else(|| anyhow::Error::msg("registry must be a table"))?
                .entry(part.clone())
                .or_insert_with(|| serde_json::json!({}));
        } else if index < 2 {
            node = node
                .get_mut(part)
                .ok_or_else(|| anyhow::Error::msg("registry override target does not exist"))?;
        } else {
            let map = node
                .as_object_mut()
                .ok_or_else(|| anyhow::Error::msg("registry override parent must be a table"))?;
            node = map.entry(part.clone()).or_insert(Value::Null);
            if node.is_null() {
                *node = if part == "auth" {
                    serde_json::json!({"mode":"api_key"})
                } else {
                    serde_json::json!({})
                };
            }
        }
    }
    let leaf = parts
        .last()
        .ok_or_else(|| anyhow::Error::msg("missing registry override field"))?;
    let map = node
        .as_object_mut()
        .ok_or_else(|| anyhow::Error::msg("registry override parent must be a table"))?;
    if value.is_null() {
        map.remove(leaf);
    } else {
        map.insert(leaf.clone(), value);
    }
    if let Some(saved) = saved
        && let Some(parents) = saved["missing_parents"].as_array()
    {
        // Restore absent optional tables without erasing edits to sibling fields.
        for parent in parents.iter().filter_map(Value::as_str) {
            let removable = document
                .pointer(parent)
                .and_then(Value::as_object)
                .is_some_and(|map| {
                    map.is_empty()
                        || (parent.ends_with("/auth")
                            && map.len() == 1
                            && map.get("mode").and_then(Value::as_str) == Some("api_key"))
                });
            if removable
                && let Some((ancestor, leaf)) = parent.rsplit_once('/')
                && let Some(map) = document
                    .pointer_mut(ancestor)
                    .and_then(Value::as_object_mut)
            {
                map.remove(&leaf.replace("~1", "/").replace("~0", "~"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Config {
        Config {
            providers: serde_json::from_value(serde_json::json!({
                "connections": {"gateway.Prod": {"kind": "custom", "uri": "https://example.invalid/v1"}},
                "models": {"chat.Fast.v2": {"capability": "chat", "provider": "gateway.Prod", "model": "wire-model"}}
            })).unwrap(),
            ..Config::default()
        }
    }

    #[test]
    fn exact_dotted_keys_and_clear_round_trip_without_materializing_defaults() {
        let mut config = fixture();
        let before = serde_json::to_value(&config.providers).unwrap();
        let path = path(
            &config,
            "providers__models__chat.Fast.v2__options__temperature",
        )
        .unwrap();
        assert_eq!(path, "/providers/models/chat.Fast.v2/options/temperature");
        let saved = snapshot(&config, &path).unwrap();
        set(&mut config, &path, "null", false).unwrap();
        let registry = config.provider_registry().unwrap();
        assert!(matches!(
            registry
                .resolve_chat(&"chat.Fast.v2".into())
                .unwrap()
                .options
                .temperature,
            Some(crate::provider_registry::Override::Clear)
        ));
        set(&mut config, &path, &saved, true).unwrap();
        assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
    }

    #[test]
    fn temporary_auth_restores_absent_auth_without_erasing_sibling_edits() {
        let mut config = fixture();
        let key_path = path(
            &config,
            "providers__connections__gateway.Prod__auth__api_key",
        )
        .unwrap();
        let saved = snapshot(&config, &key_path).unwrap();
        set(&mut config, &key_path, "fixture-env-key", false).unwrap();
        let uri_path = path(&config, "providers__connections__gateway.Prod__uri").unwrap();
        set(&mut config, &uri_path, "https://updated.invalid/v1", false).unwrap();
        set(&mut config, &key_path, &saved, true).unwrap();
        let value = serde_json::to_value(&config.providers).unwrap();
        assert!(value["connections"]["gateway.Prod"]["auth"].is_null());
        assert_eq!(
            value["connections"]["gateway.Prod"]["uri"],
            "https://updated.invalid/v1"
        );
    }

    #[test]
    fn string_fields_preserve_json_looking_environment_values() {
        let mut config = fixture();
        for tail in [
            "providers__connections__gateway.Prod__auth__api_key",
            "providers__models__chat.Fast.v2__model",
        ] {
            let path = path(&config, tail).unwrap();
            let saved = snapshot(&config, &path).unwrap();
            for value in ["123", "false", "null", "[1,2]", "\"quoted\""] {
                set(&mut config, &path, value, false).unwrap();
                let document = serde_json::to_value(&config.providers).unwrap();
                assert_eq!(
                    document.pointer(path.strip_prefix("/providers").unwrap()),
                    Some(&Value::String(value.into()))
                );
            }
            set(&mut config, &path, &saved, true).unwrap();
        }
        let path = path(
            &config,
            "providers__models__chat.Fast.v2__options__temperature",
        )
        .unwrap();
        set(&mut config, &path, "0.25", false).unwrap();
        let document = serde_json::to_value(&config.providers).unwrap();
        assert_eq!(
            document["models"]["chat.Fast.v2"]["options"]["temperature"],
            serde_json::json!(0.25)
        );
    }

    #[test]
    fn json_options_preserve_structured_environment_values() {
        let mut config = fixture();
        for field in ["chat_template_kwargs", "provider_extra"] {
            let path = path(
                &config,
                &format!("providers__models__chat.Fast.v2__options__{field}"),
            )
            .unwrap();
            let saved = snapshot(&config, &path).unwrap();
            set(
                &mut config,
                &path,
                r#"{"enable_thinking":false,"limit":123}"#,
                false,
            )
            .unwrap();
            let value = serde_json::to_value(&config.providers).unwrap();
            assert_eq!(
                value["models"]["chat.Fast.v2"]["options"][field],
                serde_json::json!({"enable_thinking":false,"limit":123})
            );
            set(&mut config, &path, &saved, true).unwrap();
        }
    }

    #[test]
    fn batch_keeps_numeric_options_and_json_looking_strings() {
        let mut config = fixture();
        set_many(
            &mut config,
            &[
                (
                    "/providers/models/chat.Fast.v2/options/temperature".into(),
                    "0.25".into(),
                ),
                (
                    "/providers/connections/gateway.Prod/auth/api_key".into(),
                    "123".into(),
                ),
                ("/providers/models/chat.Fast.v2/model".into(), "456".into()),
            ],
        )
        .unwrap();
        let document = serde_json::to_value(&config.providers).unwrap();
        assert_eq!(
            document["models"]["chat.Fast.v2"]["options"]["temperature"],
            serde_json::json!(0.25)
        );
        assert_eq!(
            document["connections"]["gateway.Prod"]["auth"]["api_key"],
            "123"
        );
        assert_eq!(document["models"]["chat.Fast.v2"]["model"], "456");
    }

    #[test]
    fn batch_preserves_structured_looking_and_numeric_header_strings() {
        let mut config = fixture();
        set_many(
            &mut config,
            &[
                (
                    "/providers/connections/gateway.Prod/extra_headers/X-Meta".into(),
                    r#"{"tenant":"work"}"#.into(),
                ),
                (
                    "/providers/connections/gateway.Prod/extra_headers/X-Project-Id".into(),
                    "123".into(),
                ),
            ],
        )
        .unwrap();
        let document = serde_json::to_value(&config.providers).unwrap();
        assert_eq!(
            document["connections"]["gateway.Prod"]["extra_headers"]["X-Meta"],
            r#"{"tenant":"work"}"#
        );
        assert_eq!(
            document["connections"]["gateway.Prod"]["extra_headers"]["X-Project-Id"],
            "123"
        );
    }

    #[test]
    fn deferred_leaf_cannot_override_a_later_whole_auth_edit() {
        let mut config = Config {
            providers: serde_json::from_value(serde_json::json!({
                "connections": {"gemini": {"kind": "gemini"}},
                "models": {}
            }))
            .unwrap(),
            ..Config::default()
        };
        set_many(
            &mut config,
            &[
                (
                    "/providers/connections/gemini/auth/client_secret".into(),
                    "early-secret".into(),
                ),
                (
                    "/providers/connections/gemini/auth".into(),
                    r#"{"mode":"oauth","client_secret":"final-secret"}"#.into(),
                ),
            ],
        )
        .unwrap();
        let document = serde_json::to_value(&config.providers).unwrap();
        assert_eq!(
            document["connections"]["gemini"]["auth"]["client_secret"],
            "final-secret"
        );
    }

    #[test]
    fn empty_collections_are_valid_even_when_serialization_omits_them() {
        let mut config = fixture();
        config.providers.connections.insert(
            "grok.local".into(),
            serde_json::from_value(serde_json::json!({
                "kind": "grok_cli", "api": {"chat": {
                    "env_passthrough": ["PATH"], "extra_args": ["--verbose"]
                }}
            }))
            .unwrap(),
        );
        for field in ["env_passthrough", "extra_args"] {
            let path = path(
                &config,
                &format!("providers__connections__grok.local__api__chat__{field}"),
            )
            .unwrap();
            let before = serde_json::to_value(&config.providers).unwrap();
            let saved = snapshot(&config, &path).unwrap();
            set(&mut config, &path, "[]", false).unwrap();
            let value = serde_json::to_value(&config.providers).unwrap();
            assert!(
                value["connections"]["grok.local"]["api"]["chat"]
                    .get(field)
                    .is_none()
            );
            set(&mut config, &path, &saved, true).unwrap();
            assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
        }
        let path = path(&config, "providers__models__chat.Fast.v2__options").unwrap();
        set(&mut config, &path, "{}", false).unwrap();
        assert!(set(&mut config, &path, r#"{"temperatur":0.5}"#, false).is_err());
    }

    #[test]
    fn invalid_field_is_atomic_and_fallback_keeps_exact_keys() {
        let mut config = fixture();
        let before = serde_json::to_value(&config.providers).unwrap();
        let bad = path(
            &config,
            "providers__models__chat.Fast.v2__options__temperatur",
        )
        .unwrap();
        assert!(set(&mut config, &bad, "0.5", false).is_err());
        assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
        let fallback = path(&config, "providers__fallback__chat.Fast.v2").unwrap();
        let saved = snapshot(&config, &fallback).unwrap();
        set(&mut config, &fallback, "[]", false).unwrap();
        set(&mut config, &fallback, &saved, true).unwrap();
        assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
    }
}
