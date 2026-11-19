//! The V4 → V5 boundary. V4 was released with ZeroClaw 0.9.0.
//!
//! All provider registry and model-route reshaping lives here. Earlier
//! migrations and generation of the released V4 layout remain unchanged.

use anyhow::{Context, Result, bail};

/// Preserve the V4 agent/runtime-profile text-tool parsing policy before the
/// V5 retirement pass removes the old key. The V5 parser policy belongs to the
/// canonical chat model, so a strict binding is projected onto its legacy
/// provider profile and is carried through the normal provider admission.
pub(super) fn preserve_strict_tool_parsing(value: &mut toml::Value) -> Result<()> {
    let Some(root) = value.as_table_mut() else {
        return Ok(());
    };
    let strict_profiles: std::collections::HashSet<String> = root
        .get("runtime_profiles")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|profiles| profiles.iter())
        .filter_map(|(name, profile)| {
            profile
                .get("strict_tool_parsing")
                .and_then(toml::Value::as_bool)
                .filter(|strict| *strict)
                .map(|_| name.clone())
        })
        .collect();

    let Some(models) = root
        .get("providers")
        .and_then(|providers| providers.get("models"))
        .and_then(toml::Value::as_table)
    else {
        return Ok(());
    };

    let mut bindings: Vec<((String, Option<String>), bool)> = root
        .get("agents")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|agents| agents.values())
        .filter_map(toml::Value::as_table)
        .flat_map(|agent| {
            let model_provider = agent
                .get("model_provider")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            // V4 resolved this setting exclusively from the runtime profile;
            // the agent-inline key was already inert and is retired at V4.
            // Do not let that stale key override the canonical profile policy.
            let strict = agent
                .get("runtime_profile")
                .and_then(toml::Value::as_str)
                .is_some_and(|profile| strict_profiles.contains(profile));
            let mut references = Vec::new();
            if let Some(reference) =
                model_provider.and_then(|reference| legacy_model_selection(models, reference))
            {
                references.push((reference, strict));
            }
            let profile = agent
                .get("runtime_profile")
                .and_then(toml::Value::as_str)
                .and_then(|alias| {
                    root.get("runtime_profiles")
                        .and_then(|profiles| profiles.get(alias))
                });
            for route in root
                .get("model_routes")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                let hint = route.get("hint").and_then(toml::Value::as_str);
                let reachable = hint.is_some_and(|hint| {
                    let automatic = profile
                        .and_then(|profile| profile.get("auto_classify"))
                        .is_some_and(|classifier| {
                            ["simple_hint", "standard_hint", "complex_hint"]
                                .iter()
                                .any(|field| {
                                    classifier.get(*field).and_then(toml::Value::as_str)
                                        == Some(hint)
                                })
                        });
                    let classified = root.get("query_classification").is_some_and(|classifier| {
                        classifier.get("enabled").and_then(toml::Value::as_bool) == Some(true)
                            && classifier
                                .get("rules")
                                .and_then(toml::Value::as_array)
                                .is_some_and(|rules| {
                                    rules.iter().any(|rule| {
                                        rule.get("hint").and_then(toml::Value::as_str) == Some(hint)
                                    })
                                })
                    });
                    automatic || classified
                }) || profile
                    .and_then(|profile| profile.get("effort_routing"))
                    .is_some_and(|routing| {
                        ["local_hint", "cloud_hint"].iter().any(|field| {
                            routing.get(*field).and_then(toml::Value::as_str) == hint
                                && hint.is_some()
                        })
                    });
                if reachable
                    && let Some(reference) = route
                        .get("model_provider")
                        .and_then(toml::Value::as_str)
                        .and_then(|reference| legacy_model_selection(models, reference))
                {
                    references.push((reference, strict));
                }
            }
            references
        })
        .collect();

    // Provider fallbacks were also executed under the consuming agent's
    // parser policy. Traverse the legacy graph before retiring its edges.
    let mut visited = std::collections::HashSet::new();
    let mut cursor = 0;
    while cursor < bindings.len() {
        let (reference, strict) = bindings[cursor].clone();
        cursor += 1;
        if !visited.insert((reference.clone(), strict)) {
            continue;
        }
        let Some((family, alias)) = reference.0.split_once('.') else {
            continue;
        };
        for fallback in models
            .get(family)
            .and_then(|family| family.get(alias))
            .and_then(|profile| profile.get("fallback"))
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
        {
            let target = if fallback.contains('.') {
                fallback.to_owned()
            } else {
                format!("{family}.{fallback}")
            };
            if let Some(target) = legacy_model_selection(models, &target)
                && !visited.contains(&(target.clone(), strict))
            {
                bindings.push((target, strict));
            }
        }
    }

    let mut strict_refs = std::collections::HashSet::new();
    let mut non_strict_refs = std::collections::HashSet::new();
    for (reference, strict) in bindings {
        if strict {
            strict_refs.insert(reference);
        } else {
            non_strict_refs.insert(reference);
        }
    }

    let mixed: Vec<_> = strict_refs
        .intersection(&non_strict_refs)
        .cloned()
        .collect();
    if let Some(reference) = mixed.into_iter().next() {
        let reference = reference.0;
        bail!(
            "cannot migrate V4 strict_tool_parsing for model_provider {reference}: it is used by both strict and non-strict agents; set model options.extract_text_tool_calls explicitly for separate model references and retry"
        );
    }

    let Some(providers) = root
        .get_mut("providers")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(());
    };
    let Some(models) = providers
        .get_mut("models")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(());
    };
    for (reference, extract) in strict_refs
        .into_iter()
        .map(|reference| (reference, false))
        .chain(
            non_strict_refs
                .into_iter()
                .map(|reference| (reference, true)),
        )
    {
        let (reference, model_alias) = reference;
        let (family, alias) = reference
            .split_once('.')
            .context("legacy provider reference must contain a family and alias")?;
        let profile = models
            .get_mut(family)
            .and_then(toml::Value::as_table_mut)
            .and_then(|family| family.get_mut(alias))
            .and_then(toml::Value::as_table_mut)
            .with_context(|| format!("legacy model_provider {reference} is missing"))?;
        let target = if let Some(model_alias) = model_alias {
            profile
                .get_mut("models")
                .and_then(toml::Value::as_table_mut)
                .and_then(|entries| entries.get_mut(&model_alias))
                .and_then(toml::Value::as_table_mut)
                .with_context(|| {
                    format!("legacy model_provider {reference}.{model_alias} is missing")
                })?
        } else {
            profile
        };
        let provider_extra = target
            .entry("provider_extra")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(provider_extra) = provider_extra.as_table_mut() else {
            bail!(
                "cannot migrate strict_tool_parsing for {reference}: provider_extra must be a table"
            );
        };
        provider_extra.insert(
            "__zeroclaw_migration_extract_text_tool_calls".into(),
            toml::Value::Boolean(extract),
        );
    }
    Ok(())
}

fn legacy_model_selection(
    models: &toml::Table,
    model_provider: &str,
) -> Option<(String, Option<String>)> {
    let (family, alias) = model_provider.split_once('.')?;
    let family_table = models.get(family)?.as_table()?;
    if family_table.contains_key(alias) {
        return Some((format!("{family}.{alias}"), None));
    }
    // Dotted aliases are TOML keys, not path separators. Resolve the longest
    // configured alias prefix before treating the remaining suffix as a
    // historical model selector.
    let mut candidate = alias;
    while let Some((prefix, _)) = candidate.rsplit_once('.') {
        candidate = prefix;
        if family_table.contains_key(candidate) {
            let suffix = alias.strip_prefix(candidate)?.strip_prefix('.')?;
            let profile = family_table.get(candidate)?.as_table()?;
            let entries = profile.get("models")?.as_table()?;
            return entries
                .contains_key(suffix)
                .then(|| (format!("{family}.{candidate}"), Some(suffix.to_owned())));
        }
    }
    None
}

#[cfg(test)]
fn legacy_profile_reference(models: &toml::Table, model_provider: &str) -> Option<String> {
    legacy_model_selection(models, model_provider).map(|(reference, _)| reference)
}

pub(super) fn migrate(value: toml::Value) -> Result<toml::Value> {
    let mut value = value;
    migrate_profile_fallbacks(&mut value)?;
    let value = super::model_routes::migrate(value)?;
    // Typed registry materialization follows admission so malformed provider
    // aliases can still use the established section-by-section salvage path.
    super::stamp_schema_version(value, 5)
}

/// Convert the V4 profile-local fallback lists before the retirement pass
/// removes those fields. The canonical registry stores exact model references
/// in one graph, so both a provider reference and a same-family model id are
/// resolved to the corresponding opaque key when possible.
fn migrate_profile_fallbacks(value: &mut toml::Value) -> Result<()> {
    let Some(root) = value.as_table_mut() else {
        return Ok(());
    };
    let Some(providers) = root
        .get_mut("providers")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(());
    };
    let Some(models) = providers.get("models").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    let mut additions = Vec::<(String, Vec<String>)>::new();
    let mut generated = toml::Table::new();
    let mut generated_connections = toml::Table::new();
    for (family, family_value) in models {
        if family_value.get("capability").is_some() {
            continue;
        }
        let Some(family_table) = family_value.as_table() else {
            continue;
        };
        for (alias, profile_value) in family_table {
            let Some(profile) = profile_value.as_table() else {
                continue;
            };
            let source = format!("{family}.{alias}");
            let ids: Vec<_> = profile
                .get("fallback_models")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .collect();
            let provider_targets: Vec<_> = profile
                .get("fallback")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|reference| !reference.is_empty())
                .map(|reference| {
                    if reference.contains('.') {
                        reference.to_owned()
                    } else {
                        format!("{family}.{reference}")
                    }
                })
                .collect();
            if ids.is_empty() && provider_targets.is_empty() {
                continue;
            }
            let entries = profile.get("models").and_then(toml::Value::as_table);
            let selected = entries.and_then(|entries| {
                entries.get("default").or_else(|| {
                    (entries.len() == 1)
                        .then(|| entries.values().next())
                        .flatten()
                })
            });
            let selections: Vec<_> = std::iter::once((source.clone(), selected, true))
                .chain(
                    entries
                        .into_iter()
                        .flat_map(|entries| entries.iter())
                        .map(|(name, entry)| (format!("{source}.{name}"), Some(entry), false)),
                )
                .collect();
            let raw = serde_json::to_value(profile)?;
            let mut provider = format!("legacy.{source}");
            // Canonical definitions can shadow every legacy selection. In that
            // case no projection will create the source connection for us.
            if !ids.is_empty()
                && selections
                    .iter()
                    .all(|(reference, _, _)| models.contains_key(reference))
            {
                let base = provider.clone();
                let mut suffix = 1;
                while providers
                    .get("connections")
                    .and_then(|connections| connections.get(&provider))
                    .is_some()
                    || generated_connections.contains_key(&provider)
                {
                    provider = format!("{base}.{suffix}");
                    suffix += 1;
                }
                let (connection, _) = super::model_routes::legacy_model(&raw, family, None, None)?;
                generated_connections.insert(provider.clone(), toml::Value::try_from(connection)?);
            }
            for (model_source, entry, primary) in selections {
                let entry: Option<crate::schema::ModelEntryConfig> =
                    entry.cloned().map(toml::Value::try_into).transpose()?;
                let mut targets = Vec::new();
                for id in &ids {
                    let (_, mut model) =
                        super::model_routes::legacy_model(&raw, family, entry.as_ref(), Some(id))?;
                    if primary
                        && let Some(policy) = raw.get("provider_extra").and_then(|extra| {
                            extra.get("__zeroclaw_migration_extract_text_tool_calls")
                        })
                    {
                        model["options"]["extract_text_tool_calls"] = policy.clone();
                    }
                    model["provider"] = provider.clone().into();
                    let model = toml::Value::try_from(model)?;
                    // Reuse an identical materialized fallback, while keeping
                    // distinct policies (notably strict parsing) independent.
                    let reference = if let Some((reference, _)) =
                        generated.iter().find(|(_, candidate)| **candidate == model)
                    {
                        reference.clone()
                    } else {
                        let base = format!("fallback.{model_source}.{id}");
                        let mut reference = base.clone();
                        let mut suffix = 1;
                        while models.contains_key(&reference) || generated.contains_key(&reference)
                        {
                            reference = format!("{base}.{suffix}");
                            suffix += 1;
                        }
                        generated.insert(reference.clone(), model);
                        reference
                    };
                    if !targets.contains(&reference) {
                        targets.push(reference);
                    }
                }
                // V4 exhausted same-connection models before provider refs.
                for target in &provider_targets {
                    if target != &model_source && !targets.contains(target) {
                        targets.push(target.clone());
                    }
                }
                if !targets.is_empty() {
                    additions.push((model_source, targets));
                }
            }
        }
    }
    if !generated_connections.is_empty() {
        providers
            .entry("connections")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("providers.connections must be a table")?
            .extend(generated_connections);
    }
    providers
        .get_mut("models")
        .and_then(toml::Value::as_table_mut)
        .context("providers.models must be a table")?
        .extend(generated);
    if additions.is_empty() {
        return Ok(());
    }
    let fallback = providers
        .entry("fallback")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .context("providers.fallback must be a table")?;
    for (source, targets) in additions {
        let entry = fallback
            .entry(source)
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()
            .context("providers.fallback entries must be arrays")?;
        for target in targets {
            if !entry
                .iter()
                .any(|value| value.as_str() == Some(target.as_str()))
            {
                entry.push(toml::Value::String(target));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{GenerateOptions, generate, migrate_file};
    use super::legacy_profile_reference;

    #[test]
    fn released_v4_generation_stays_frozen() {
        assert_eq!(
            generate(4, &GenerateOptions::default()).unwrap(),
            include_str!("../../fixtures/v4.toml"),
        );
    }

    #[test]
    fn current_v5_generation_matches_fixture() {
        assert_eq!(
            generate(5, &GenerateOptions::default()).unwrap(),
            include_str!("../../fixtures/v5.toml"),
        );
    }

    #[test]
    fn v4_to_v5_preserves_values_comments_and_secret_ciphertext() {
        let input = "schema_version = 4 # released version\n\n[providers.models.openai.main]\napi_key = 'enc2:TEST_CIPHERTEXT'\nmodel = 'gpt-4o'\n\n[tools]\noptional = ['*']\n";
        let output = migrate_file(input).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(value["schema_version"].as_integer(), Some(5));
        assert!(value["providers"].get("connections").is_some());
        assert_eq!(
            value["providers"]["models"]["openai.main"]["model"].as_str(),
            Some("gpt-4o")
        );
        assert!(output.contains("enc2:TEST_CIPHERTEXT"));
        assert!(value["providers"]["models"].get("openai").is_none());
        assert!(migrate_file(&output).unwrap().is_none());
    }

    #[test]
    fn v4_provider_profiles_become_v5_connections_and_models() {
        let input = "schema_version = 4\n\n[providers.models.openai.main]\napi_key = 'enc2:TEST_CIPHERTEXT'\nmodel = 'gpt-4o'\n\n[tools]\noptional = ['*']\n";
        let output = migrate_file(input).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(value["schema_version"].as_integer(), Some(5));
        assert!(value["providers"].get("connections").is_some());
        assert_eq!(
            value["providers"]["models"]["openai.main"]["model"].as_str(),
            Some("gpt-4o")
        );
        assert!(output.contains("enc2:TEST_CIPHERTEXT"));
        assert!(value["providers"]["models"].get("openai").is_none());
        assert!(migrate_file(&output).unwrap().is_none());
    }

    #[test]
    fn strict_runtime_profile_becomes_model_text_tool_opt_out() {
        let input = r#"
schema_version = 4

[providers.models.openai.main]
api_key = "test-key"
model = "gpt-4o"

[agents.worker]
model_provider = "openai.main"
runtime_profile = "strict"

[runtime_profiles.strict]
strict_tool_parsing = true
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(
            value["providers"]["models"]["openai.main"]["options"]["extract_text_tool_calls"]
                .as_bool(),
            Some(false)
        );
        assert!(!output.contains("strict_tool_parsing"));
    }

    #[test]
    fn strict_and_non_strict_agents_sharing_a_model_are_rejected() {
        let input = r#"
schema_version = 4

[providers.models.openai.main]
api_key = "test-key"
model = "gpt-4o"

[agents.strict]
model_provider = "openai.main"
runtime_profile = "strict"

[agents.default]
model_provider = "openai.main"

[runtime_profiles.strict]
strict_tool_parsing = true
"#;
        let error = migrate_file(input).unwrap_err().to_string();
        assert!(
            error.contains("both strict and non-strict agents"),
            "{error}"
        );
    }

    #[test]
    fn strict_nested_model_binding_preserves_text_tool_opt_out() {
        let input = r#"
schema_version = 4

[providers.models.ollama.main]
model = "llama3"
[providers.models.ollama.main.models.fast]
id = "llama3-fast"

[agents.worker]
model_provider = "ollama.main.fast"
runtime_profile = "strict"

[runtime_profiles.strict]
strict_tool_parsing = true
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(
            value["providers"]["models"]["ollama.main.fast"]["options"]["extract_text_tool_calls"]
                .as_bool(),
            Some(false)
        );
        assert!(!output.contains("strict_tool_parsing"));
    }

    #[test]
    fn fallback_model_ids_are_materialized_before_provider_fallbacks() {
        let input = r#"
schema_version = 4

[providers.models.openai.main]
model = "gpt-4o"
fallback_models = ["gpt-4o-mini"]
fallback = ["openai.backup"]

[providers.models.openai.backup]
model = "gpt-4o-backup"
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let value: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(
            value["providers"]["models"]["openai.main"]["model"].as_str(),
            Some("gpt-4o")
        );
        assert_eq!(
            value["providers"]["models"]["fallback.openai.main.gpt-4o-mini"]["model"].as_str(),
            Some("gpt-4o-mini")
        );
        assert_eq!(
            value["providers"]["fallback"]["openai.main"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>(),
            vec!["fallback.openai.main.gpt-4o-mini", "openai.backup"]
        );
    }

    #[test]
    fn fallback_generation_keeps_v4_sole_entry_selection_and_named_fallbacks() {
        let input = r#"
schema_version = 4
[providers.models.custom.main]
model = "profile-model"
uri = "https://example.test/v1"
fallback_models = ["backup-model"]
[providers.models.custom.main.models.fast]
id = "selected-model"
temperature = 0.2
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let config: crate::schema::Config = toml::from_str(&output).unwrap();
        assert_eq!(
            config.resolve_model_id("custom.main").as_deref(),
            Some("selected-model")
        );
        let registry = config.provider_registry().unwrap();
        for source in ["custom.main", "custom.main.fast"] {
            let targets = registry.fallback_chain(&source.into()).unwrap();
            assert_eq!(targets.len(), 1);
            let fallback = registry.resolve_chat(&targets[0]).unwrap();
            assert_eq!(fallback.upstream_model(), Some("backup-model"));
            assert_eq!(
                fallback.selection.connection.uri(),
                Some("https://example.test/v1")
            );
        }
    }

    #[test]
    fn strict_primary_does_not_change_sibling_text_tools() {
        let input = r#"
schema_version = 4
[providers.models.ollama.main]
model = "primary"
[providers.models.ollama.main.models.default]
id = "primary"
[providers.models.ollama.main.models.fast]
id = "fast"
[agents.strict]
model_provider = "ollama.main"
runtime_profile = "strict"
[agents.normal]
model_provider = "ollama.main.fast"
runtime_profile = "normal"
[runtime_profiles.strict]
strict_tool_parsing = true
[runtime_profiles.normal]
strict_tool_parsing = false
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let config: crate::schema::Config = toml::from_str(&output).unwrap();
        assert!(!config.model_text_extraction("ollama.main").tool_calls);
        assert!(config.model_text_extraction("ollama.main.fast").tool_calls);
    }

    #[test]
    fn named_routes_keep_their_own_policy_for_dotted_profile_keys() {
        for strict_primary in [false, true] {
            let input = format!(
                r#"
schema_version = 4
[providers.models.ollama."main.us"]
model = "primary"
[providers.models.ollama."main.us".models.fast]
id = "fast"
[agents.primary]
model_provider = "ollama.main.us"
runtime_profile = "route_policy"
[agents.named]
model_provider = "ollama.main.us.fast"
runtime_profile = "route_policy"
[runtime_profiles.route_policy]
strict_tool_parsing = {strict_primary}
[[model_routes]]
hint = "fast"
model_provider = "ollama.main.us.fast"
"#,
            );
            let config = crate::migration::migrate_to_current(&input).unwrap();
            let route = &config.model_routes[0].model_ref;
            assert_eq!(
                config.model_text_extraction(route).tool_calls,
                !strict_primary
            );
            assert_eq!(
                config
                    .model_text_extraction("ollama.main.us.fast")
                    .tool_calls,
                !strict_primary
            );
        }
    }

    #[test]
    fn strict_classifier_migrates_only_its_reachable_routes() {
        let input = r#"
schema_version = 4
[providers.models.ollama.primary]
model = "primary"
[providers.models.ollama.fast]
model = "fast"
[providers.models.ollama.deep]
model = "deep"
[agents.strict]
model_provider = "ollama.primary"
runtime_profile = "strict"
[agents.normal]
model_provider = "ollama.deep"
[runtime_profiles.strict]
strict_tool_parsing = true
[runtime_profiles.strict.auto_classify]
simple_hint = "fast"
[[model_routes]]
hint = "fast"
model_provider = "ollama.fast"
[[model_routes]]
hint = "deep"
model_provider = "ollama.deep"
"#;
        let config = crate::migration::migrate_to_current(input).unwrap();
        for route in &config.model_routes {
            assert_eq!(
                config.model_text_extraction(&route.model_ref).tool_calls,
                route.hint == "deep"
            );
        }
    }

    #[test]
    fn non_strict_custom_agent_keeps_text_tools_without_native_tools_setting() {
        let input = r#"
schema_version = 4
[providers.models.custom.main]
model = "chat"
uri = "https://example.test/v1"
[agents.worker]
model_provider = "custom.main"
"#;
        let output = migrate_file(input).unwrap().unwrap();
        let config: crate::schema::Config = toml::from_str(&output).unwrap();
        assert!(config.model_text_extraction("custom.main").tool_calls);
    }

    #[test]
    fn named_strict_fallback_keeps_its_own_parser_and_sampling_policy() {
        let input = r#"
schema_version = 4
[providers.models.ollama.main]
model = "primary"
fallback_models = ["backup"]
[providers.models.ollama.main.models.fast]
id = "fast"
temperature = 0.2
[agents.strict]
model_provider = "ollama.main.fast"
runtime_profile = "strict"
[agents.normal]
model_provider = "ollama.main"
[runtime_profiles.strict]
strict_tool_parsing = true
"#;
        let config = crate::migration::migrate_to_current(input).unwrap();
        let registry = config.provider_registry().unwrap();
        let strict = registry.fallback_chain(&"ollama.main.fast".into()).unwrap();
        let normal = registry.fallback_chain(&"ollama.main".into()).unwrap();
        assert_ne!(strict[0], normal[0]);
        assert!(!config.model_text_extraction(strict[0].as_str()).tool_calls);
        assert!(config.model_text_extraction(normal[0].as_str()).tool_calls);
        assert_eq!(
            config.resolve_model_temperature(strict[0].as_str()),
            Some(0.2)
        );
    }

    #[test]
    fn shadowed_legacy_primary_still_materializes_its_fallback_connection() {
        let input = r#"
schema_version = 4
[providers.connections.shared]
kind = "openai"
[providers.models."openai.main"]
capability = "chat"
provider = "shared"
model = "canonical"
[providers.models.openai.main]
model = "legacy"
uri = "https://legacy.example.test/v1"
fallback_models = ["backup"]
"#;
        let config = crate::migration::migrate_to_current(input).unwrap();
        let registry = config.provider_registry().unwrap();
        assert_eq!(
            registry
                .resolve_chat(&"openai.main".into())
                .unwrap()
                .upstream_model(),
            Some("canonical")
        );
        let fallback = registry.fallback_chain(&"openai.main".into()).unwrap();
        let backup = registry.resolve_chat(&fallback[0]).unwrap();
        assert_eq!(backup.upstream_model(), Some("backup"));
        assert_eq!(
            backup.selection.connection.uri(),
            Some("https://legacy.example.test/v1")
        );
    }

    #[test]
    fn strict_provider_fallback_shared_with_non_strict_agent_requires_explicit_split() {
        let input = r#"
schema_version = 4
[providers.models.ollama.main]
model = "primary"
fallback = ["backup"]
[providers.models.ollama.backup]
model = "backup"
[agents.strict]
model_provider = "ollama.main"
runtime_profile = "strict"
[agents.normal]
model_provider = "ollama.backup"
[runtime_profiles.strict]
strict_tool_parsing = true
"#;
        let error = migrate_file(input).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("both strict and non-strict agents"),
            "{error:#}"
        );
    }

    #[test]
    fn legacy_strict_profile_lookup_uses_longest_exact_toml_key() {
        let models: toml::Value = toml::from_str(
            r#"
[ollama.main]
[ollama.main.models.fast]
[ollama."main.us"]
[ollama."main.us".models.fast]
"#,
        )
        .unwrap();
        for (input, expected) in [
            ("ollama.main.fast", Some("ollama.main")),
            ("ollama.main.us.fast", Some("ollama.main.us")),
            ("ollama.main.us", Some("ollama.main.us")),
            ("ollama.unknown.fast", None),
        ] {
            assert_eq!(
                legacy_profile_reference(models.as_table().unwrap(), input).as_deref(),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn malformed_v4_provider_does_not_discard_unrelated_config_during_salvage() {
        let input = "schema_version = 4\nlocale = 'fr'\n\n[security]\ntrust_daemon_uid = false\n\n[providers.models.openai.main]\napi_key = 123\n";
        assert!(super::super::migrate_to_current(input).is_err());
        let load = super::super::migrate_to_current_salvaged(input);
        assert_eq!(load.config.schema_version, 5);
        assert_eq!(load.config.locale.as_deref(), Some("fr"));
        assert!(!load.config.security.trust_daemon_uid);
        assert!(load.dropped_security.is_empty());
    }

    #[test]
    fn older_versions_reach_v5_through_the_chain() {
        for version in 1..=4 {
            let source = generate(version, &GenerateOptions::default()).unwrap();
            let output = migrate_file(&source).unwrap().unwrap();
            let value: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(value["schema_version"].as_integer(), Some(5));
            assert!(migrate_file(&output).unwrap().is_none());
        }
    }
}
