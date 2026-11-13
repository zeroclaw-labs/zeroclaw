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
        if document.pointer(path).is_none() {
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
        } else if original.is_null() && admitted.is_object() {
            for (key, value) in admitted.as_object().into_iter().flatten() {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                differences(&Value::Null, value, &path, out);
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
    let mut document = serde_json::to_value(&config.providers)?;
    if restore {
        restore_document(&mut document, path, value)?;
        config.providers = serde_json::from_value(document)?;
        return Ok(());
    }
    // Let the real schema select the type. String fields must preserve values
    // such as numeric model IDs and JSON-looking credentials verbatim.
    let literal = if value.is_empty() {
        Value::Null
    } else {
        Value::String(value.to_owned())
    };
    // JSON-valued options need their structured representation. A typed string
    // field rejects numbers/objects here, then accepts the unchanged literal
    // below. Keep strings (including quoted credentials) and the clear
    // sentinel verbatim instead of treating them as JSON string syntax.
    let parsed = serde_json::from_str::<Value>(value)
        .ok()
        .filter(|value| !value.is_null() && !value.is_string())
        .unwrap_or_else(|| literal.clone());
    set_document(&mut document, path, parsed.clone(), None)?;
    let candidate = serde_json::from_value(document.clone());
    let candidate: crate::providers::Providers = match candidate {
        Ok(candidate) => candidate,
        Err(error) => {
            ensure!(parsed != literal, "{error}");
            set_document(&mut document, path, literal, None)?;
            serde_json::from_value(document)?
        }
    };
    // Config files report unknown options at startup. Environment overrides
    // must fail atomically if their target disappears during typed decoding.
    if !value.is_empty() {
        let pointer = path
            .strip_prefix("/providers")
            .ok_or_else(|| anyhow::Error::msg("invalid registry override path"))?;
        ensure!(
            serde_json::to_value(&candidate)?.pointer(pointer).is_some(),
            "unknown registry override field"
        );
    }
    config.providers = candidate;
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
    ensure!(parts.len() >= 2, "override must target a registry leaf");
    let mut node = &mut *document;
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
