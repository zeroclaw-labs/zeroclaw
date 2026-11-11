//! Exact-key mutations shared by non-UI registry editors.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::schema::Config;

fn document(config: &Config) -> Result<Value> {
    let mut document = serde_json::to_value(config.provider_registry()?)?;
    for section in ["connections", "models", "model_presets", "fallback"] {
        if document.get(section).is_none() {
            document[section] = serde_json::json!({});
        }
    }
    Ok(document)
}

fn entries<'a>(
    document: &'a mut Value,
    section: &str,
) -> Result<&'a mut serde_json::Map<String, Value>> {
    ensure!(
        matches!(
            section,
            "connections" | "models" | "model_presets" | "fallback"
        ),
        "unknown provider registry"
    );
    document
        .get_mut(section)
        .and_then(Value::as_object_mut)
        .context("registry must be a table")
}

pub fn create_entry(
    config: &mut Config,
    section: &str,
    key: &str,
    definition: Value,
) -> Result<()> {
    ensure!(!key.trim().is_empty(), "registry key must not be empty");
    let expected = super::snapshot(config)?;
    let mut registry = document(config)?;
    let entries = entries(&mut registry, section)?;
    ensure!(!entries.contains_key(key), "registry entry already exists");
    entries.insert(key.to_owned(), definition);
    super::stage(config, super::RegistryEdit { expected, registry })
}

fn check_env_reference(config: &Config, section: &str, key: &str) -> Result<()> {
    let prefix = format!(
        "/providers/{section}/{}",
        key.replace('~', "~0").replace('/', "~1")
    );
    ensure!(
        !config
            .env_overridden_paths
            .iter()
            .any(|path| path == &prefix
                || path
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|tail| tail.starts_with('/'))),
        "registry entry has environment overrides; update the environment before renaming or deleting it"
    );
    Ok(())
}

pub fn delete_entry(config: &mut Config, section: &str, key: &str) -> Result<()> {
    let mut candidate = config.clone();
    // Normalize legacy consumers before evaluating references to a canonical key.
    super::update(&mut candidate, |_| Ok(()))?;
    check_env_reference(&candidate, section, key)?;
    let expected = super::snapshot(&candidate)?;
    let mut registry = document(&candidate)?;
    ensure!(
        entries(&mut registry, section)?.remove(key).is_some(),
        "registry entry does not exist"
    );
    // Stage refuses every dangling model, connection, preset and consumer.
    super::stage(&mut candidate, super::RegistryEdit { expected, registry })?;
    *config = candidate;
    Ok(())
}

pub fn rename_entry(config: &mut Config, section: &str, from: &str, to: &str) -> Result<()> {
    ensure!(!to.trim().is_empty(), "registry key must not be empty");
    let mut candidate = config.clone();
    super::update(&mut candidate, |_| Ok(()))?;
    let before = candidate.clone();
    check_env_reference(&candidate, section, from)?;
    let expected = super::snapshot(&candidate)?;
    let mut registry = document(&candidate)?;
    let table = entries(&mut registry, section)?;
    ensure!(table.contains_key(from), "registry entry does not exist");
    if from == to {
        return Ok(());
    }
    ensure!(
        !table.contains_key(to),
        "destination registry entry already exists"
    );
    let value = table.remove(from).context("registry entry disappeared")?;
    table.insert(to.to_owned(), value);
    match section {
        "connections" => {
            for model in registry["models"]
                .as_object_mut()
                .into_iter()
                .flat_map(|models| models.values_mut())
            {
                replace_value(model.get_mut("provider"), from, to);
            }
        }
        "model_presets" => {
            for model in registry["models"]
                .as_object_mut()
                .into_iter()
                .flat_map(|models| models.values_mut())
            {
                replace_value(model.get_mut("preset"), from, to);
            }
            for connection in registry["connections"]
                .as_object_mut()
                .into_iter()
                .flat_map(|connections| connections.values_mut())
            {
                for api in connection
                    .get_mut("api")
                    .and_then(Value::as_object_mut)
                    .into_iter()
                    .flat_map(|apis| apis.values_mut())
                {
                    replace_value(api.get_mut("preset"), from, to);
                }
            }
        }
        "models" => {
            let graph = registry["fallback"]
                .as_object_mut()
                .context("fallback registry must be a table")?;
            if let Some(targets) = graph.remove(from) {
                graph.insert(to.to_owned(), targets);
            }
            for targets in graph.values_mut().filter_map(Value::as_array_mut) {
                for target in targets {
                    replace_value(Some(target), from, to);
                }
            }
            rewrite_consumers(&mut candidate, from, to);
        }
        _ => {}
    }
    super::stage(&mut candidate, super::RegistryEdit { expected, registry })?;
    // References can themselves be environment-owned (an agent binding or a
    // model's connection/preset). Never rewrite only their persisted half.
    for changed in Config::semantic_changed_paths(&before, &candidate)? {
        let dotted = changed.join(".");
        let managed = before.env_overridden_paths.iter().any(|path| {
            if let Some(pointer) = path.strip_prefix('/') {
                let segments: Vec<_> = pointer
                    .split('/')
                    .map(|part| part.replace("~1", "/").replace("~0", "~"))
                    .collect();
                segments.starts_with(&changed) || changed.starts_with(&segments)
            } else {
                path == &dotted
                    || path.starts_with(&format!("{dotted}."))
                    || dotted.starts_with(&format!("{path}."))
            }
        });
        ensure!(
            !managed,
            "rename would change an environment-owned reference; update the environment first"
        );
    }
    *config = candidate;
    Ok(())
}

fn replace_value(value: Option<&mut Value>, from: &str, to: &str) {
    if let Some(value) = value
        && value.as_str() == Some(from)
    {
        *value = to.into();
    }
}

fn replace<T: AsRef<str> + From<String>>(value: &mut T, from: &str, to: &str) -> bool {
    if value.as_ref().trim() != from {
        return false;
    }
    *value = to.to_owned().into();
    true
}

fn rewrite_consumers(config: &mut Config, from: &str, to: &str) {
    let mut agents = false;
    for agent in config.agents.values_mut() {
        agents |= replace(&mut agent.model_provider, from, to)
            | replace(&mut agent.classifier_provider, from, to)
            | replace(&mut agent.summary_provider, from, to)
            | replace(&mut agent.tts_provider, from, to)
            | replace(&mut agent.transcription_provider, from, to);
    }
    if agents {
        config.mark_dirty("agents");
    }
    let mut profiles = false;
    for profile in config.runtime_profiles.values_mut() {
        profiles |= replace(&mut profile.context_compression.summary_provider, from, to);
    }
    if profiles {
        config.mark_dirty("runtime_profiles");
    }
    let mut routes = false;
    for route in &mut config.model_routes {
        routes |= replace(&mut route.model_ref, from, to);
    }
    if routes {
        config.mark_dirty("model_routes");
    }
    let mut embedding_routes = false;
    for route in &mut config.embedding_routes {
        embedding_routes |= replace(&mut route.model_provider, from, to);
    }
    if embedding_routes {
        config.mark_dirty("embedding_routes");
    }
    if config
        .memory
        .embedding_model_ref
        .as_mut()
        .is_some_and(|reference| replace(reference, from, to))
    {
        config.mark_dirty("memory.embedding_model_ref");
    }
    if config
        .multimodal
        .vision_model_provider
        .as_mut()
        .is_some_and(|reference| replace(reference, from, to))
    {
        config.mark_dirty("multimodal.vision_model_provider");
    }
}
