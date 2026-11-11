use serde_json::json;
use zeroclaw_config::provider_registry::editor::{RegistryEdit, snapshot, stage};
use zeroclaw_config::schema::Config;

fn config() -> Config {
    let providers = toml::from_str(
        r#"
[connections."gateway.prod"]
kind = "custom"
uri = "https://example.test/v1"
auth = { mode = "api_key", api_key = "test-secret" }
[connections."gateway.prod".extra_headers]
Authorization = "test-header"
X-Custom-Auth = "custom-header-secret"
[models."chat.fast.us"]
capability = "chat"
provider = "gateway.prod"
model = "model-one"
[models."chat.backup"]
capability = "chat"
provider = "gateway.prod"
model = "model-two"
[fallback]
"chat.fast.us" = ["chat.backup"]
"#,
    )
    .unwrap();
    let mut config = Config {
        providers,
        ..Config::default()
    };
    config
        .providers
        .models
        .ensure("openai", "legacy.prod")
        .unwrap()
        .model = Some("legacy-model".into());
    config
}

#[test]
fn registry_save_promotes_legacy_embedding_consumers_and_reuses_the_connection() {
    let mut config = config();
    config.memory.embedding_provider = "openai.legacy.prod".into();
    config.memory.embedding_model = "text-embedding-3-small".into();
    config.memory.embedding_dimensions = 1536;
    config
        .embedding_routes
        .push(zeroclaw_config::schema::EmbeddingRouteConfig {
            hint: "semantic.v1".into(),
            model_provider: "openai.legacy.prod".into(),
            model: "text-embedding-3-large".into(),
            dimensions: Some(3072),
            api_key: Some("route-secret".into()),
        });
    let expected = snapshot(&config).unwrap();
    stage(
        &mut config,
        RegistryEdit {
            registry: expected.clone(),
            expected,
        },
    )
    .unwrap();
    assert!(config.providers.models.openai.is_empty());
    let registry = config.provider_registry().unwrap();
    let memory_ref = config.memory.embedding_model_ref.as_deref().unwrap();
    let memory = registry.resolve_embedding(&memory_ref.into()).unwrap();
    assert_eq!(memory.model, "text-embedding-3-small");
    assert_eq!(memory.dimensions, Some(1536));
    let route = registry
        .resolve_embedding(&config.embedding_routes[0].model_provider.as_str().into())
        .unwrap();
    assert_eq!(route.model, "text-embedding-3-large");
    assert_eq!(route.dimensions, Some(3072));
    assert_eq!(
        route.selection.model.provider(),
        memory.selection.model.provider()
    );
    assert_eq!(
        config.embedding_routes[0].api_key.as_deref(),
        Some("route-secret")
    );
    let before = snapshot(&config).unwrap();
    stage(
        &mut config,
        RegistryEdit {
            registry: before.clone(),
            expected: before.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        snapshot(&config).unwrap(),
        before,
        "subsequent saves must not regenerate models"
    );
    let reloaded: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    reloaded.validate().unwrap();
}

#[test]
fn registry_save_preserves_embeddings_on_chat_only_compatible_endpoints() {
    use zeroclaw_api::capability::Capability;

    let mut config = config();
    let profile = config
        .providers
        .models
        .ensure("deepseek", "gateway.prod")
        .unwrap();
    profile.model = Some("chat-model".into());
    profile.uri = Some("https://old.example.test/v1".into());
    profile.api_key = Some("embedding-secret".into());
    profile
        .extra_headers
        .insert("X-Private".into(), "header-secret".into());
    profile.tls_ca_cert_path = Some("/tmp/test-ca.pem".into());
    config.memory.embedding_provider = "deepseek.gateway.prod".into();
    config.memory.embedding_model = "embedding-small".into();
    config.memory.embedding_dimensions = 1536;
    config
        .embedding_routes
        .push(zeroclaw_config::schema::EmbeddingRouteConfig {
            hint: "semantic.v1".into(),
            model_provider: "deepseek.gateway.prod".into(),
            model: "embedding-large".into(),
            dimensions: Some(3072),
            api_key: None,
        });
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["connections"]["legacy.deepseek.gateway.prod"]["uri"] =
        json!("https://edited.example.test/v1");
    registry["connections"]["legacy.deepseek.gateway.prod"]["auth"]["api_key"] =
        json!("edited-secret");
    // A user-owned connection at the generated name must not be overwritten.
    registry["connections"]["embedding.legacy.deepseek.gateway.prod"] = json!({
        "kind": "custom", "uri": "https://other.example.test/v1"
    });
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();

    let registry = config.provider_registry().unwrap();
    let memory = registry
        .resolve_embedding(&config.memory.embedding_model_ref.as_deref().unwrap().into())
        .unwrap();
    let route = registry
        .resolve_embedding(&config.embedding_routes[0].model_provider.as_str().into())
        .unwrap();
    assert_eq!(memory.model, "embedding-small");
    assert_eq!(memory.dimensions, Some(1536));
    assert_eq!(route.model, "embedding-large");
    assert_eq!(route.dimensions, Some(3072));
    assert_eq!(
        memory.selection.model.provider(),
        route.selection.model.provider()
    );
    assert_eq!(
        memory.selection.model.provider().as_str(),
        "embedding.legacy.deepseek.gateway.prod.2"
    );
    let connection = memory.selection.connection;
    assert_eq!(connection.kind(), "custom");
    assert_eq!(connection.capabilities().len(), 1);
    assert!(connection.supports(Capability::Embedding));
    assert_eq!(connection.uri(), Some("https://edited.example.test/v1"));
    assert_eq!(connection.api_key(), Some("edited-secret"));
    assert_eq!(
        connection.extra_headers().unwrap()["X-Private"],
        "header-secret"
    );
    assert_eq!(connection.tls_ca_cert_path(), Some("/tmp/test-ca.pem"));
    let chat = &registry.connections["legacy.deepseek.gateway.prod"];
    assert_eq!(chat.kind(), "deepseek");
    assert!(!chat.supports(Capability::Embedding));
    assert_eq!(
        registry.connections["embedding.legacy.deepseek.gateway.prod"].uri(),
        Some("https://other.example.test/v1")
    );

    let before = snapshot(&config).unwrap();
    stage(
        &mut config,
        RegistryEdit {
            registry: before.clone(),
            expected: before.clone(),
        },
    )
    .unwrap();
    assert_eq!(snapshot(&config).unwrap(), before);
    let reloaded: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    reloaded.validate().unwrap();
}

#[test]
fn registry_save_does_not_bypass_explicitly_disabled_embeddings() {
    let mut config = config();
    config.memory.embedding_provider = "openai.legacy.prod".into();
    config.memory.embedding_model = "embedding-small".into();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["connections"]["legacy.openai.legacy.prod"]["capabilities"] = json!(["chat"]);
    let error = stage(
        &mut config,
        RegistryEdit {
            expected: expected.clone(),
            registry,
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("does not support embeddings"),
        "{error}"
    );
    assert_eq!(snapshot(&config).unwrap(), expected);
}

#[test]
fn registry_save_rejects_deleting_a_legacy_embedding_connection_atomically() {
    let mut config = config();
    config.memory.embedding_provider = "openai.legacy.prod".into();
    config.memory.embedding_model = "text-embedding-3-small".into();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["connections"]
        .as_object_mut()
        .unwrap()
        .remove("legacy.openai.legacy.prod");
    registry["models"]
        .as_object_mut()
        .unwrap()
        .remove("openai.legacy.prod");
    assert!(
        stage(
            &mut config,
            RegistryEdit {
                expected: expected.clone(),
                registry
            }
        )
        .is_err()
    );
    assert_eq!(snapshot(&config).unwrap(), expected);
}

#[test]
fn connection_presets_require_matching_capabilities_even_without_models() {
    use zeroclaw_config::provider_registry::ProviderRegistry;

    for capability in ["chat", "tts", "asr"] {
        let mut value = json!({
            "connections": {
                "gateway.prod": {"kind": "custom", "api": {
                    (capability): {"preset": "defaults.prod"}
                }}
            },
            "model_presets": {
                "defaults.prod": {"capability": capability, "options": {}}
            }
        });
        serde_json::from_value::<ProviderRegistry>(value.clone())
            .expect("matching API preset should validate without any models");
        value["model_presets"]["defaults.prod"]["capability"] =
            json!(if capability == "asr" { "chat" } else { "asr" });
        let mismatch = serde_json::from_value::<ProviderRegistry>(value.clone()).unwrap_err();
        assert!(
            mismatch
                .to_string()
                .contains(&format!("api.{capability}.preset")),
            "{mismatch}"
        );
        assert!(mismatch.to_string().contains("capability"), "{mismatch}");
        value["model_presets"] = json!({});
        let missing = serde_json::from_value::<ProviderRegistry>(value).unwrap_err();
        assert!(
            missing.to_string().contains("preset does not exist"),
            "{missing}"
        );
    }
}

#[test]
fn registry_editor_rejects_deleting_a_connection_preset_atomically() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["model_presets"] = json!({"voice.default": {"capability": "tts", "options": {}}});
    registry["connections"]["gateway.prod"]["api"] = json!({"tts": {"preset": "voice.default"}});
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["model_presets"] = json!({});
    let result = stage(
        &mut config,
        RegistryEdit {
            expected: expected.clone(),
            registry,
        },
    );
    assert!(
        result.is_err(),
        "a referenced connection preset cannot be deleted"
    );
    assert_eq!(snapshot(&config).unwrap(), expected);
}

#[test]
fn chat_defaults_apply_to_driver_specific_api_objects() {
    use zeroclaw_config::provider_registry::{Override, ProviderRegistry};
    for kind in [
        "custom",
        "gemini_cli",
        "kilocli",
        "grok_cli",
        "hailo_ollama",
    ] {
        let registry: ProviderRegistry = serde_json::from_value(json!({
            "connections": {"gateway.prod": {"kind": kind, "api": {"chat": {
                "preset": "chat.defaults", "options": {"temperature": 0.4}
            }}}},
            "model_presets": {"chat.defaults": {"capability": "chat", "options": {
                "temperature": 0.2, "extract_text_reasoning": false
            }}},
            "models": {"chat.fast.us": {"capability": "chat", "provider": "gateway.prod", "model": "model-one"}}
        })).unwrap();
        let model_ref = "chat.fast.us".into();
        let resolved = registry.resolve_chat(&model_ref).unwrap();
        assert_eq!(
            resolved.options.temperature,
            Some(Override::Set(0.4)),
            "{kind}"
        );
        assert_eq!(
            resolved.options.extract_text_reasoning,
            Some(false),
            "{kind}"
        );
    }
}

#[test]
fn registry_editor_preserves_exact_keys_and_secrets_while_promoting_legacy_maps() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    assert!(!expected.to_string().contains("test-secret"));
    assert!(!expected.to_string().contains("test-header"));
    assert!(!expected.to_string().contains("custom-header-secret"));
    let mut registry = expected.clone();
    registry["models"]["chat.fast.us"]["model"] = json!("model-updated");
    registry["models"]["chat.fast.us"]["options"] = json!({"temperature": "null"});
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    assert_eq!(
        config.resolve_model_id("chat.fast.us").as_deref(),
        Some("model-updated")
    );
    assert_eq!(
        config.resolve_model_id("openai.legacy.prod").as_deref(),
        Some("legacy-model")
    );
    let value = serde_json::to_value(&config.providers).unwrap();
    assert_eq!(
        value["connections"]["gateway.prod"]["auth"]["api_key"],
        "test-secret"
    );
    assert_eq!(
        value["connections"]["gateway.prod"]["extra_headers"]["Authorization"],
        "test-header"
    );
    assert_eq!(
        snapshot(&config).unwrap()["models"]["chat.fast.us"]["options"]["temperature"],
        "null"
    );
    assert!(config.providers.models.iter_entries().next().is_none());
    let encoded = toml::to_string(&config.providers).unwrap();
    let reloaded = Config {
        providers: toml::from_str(&encoded).unwrap(),
        ..Config::default()
    };
    assert_eq!(snapshot(&reloaded).unwrap(), snapshot(&config).unwrap());
}

#[test]
fn registry_editor_promotes_masked_legacy_credentials_and_keeps_later_edits() {
    let mut config = config();
    config
        .providers
        .models
        .ensure("openai", "legacy.prod")
        .unwrap()
        .api_key = Some("legacy-test-secret".into());
    let expected = snapshot(&config).unwrap();
    assert!(!expected.to_string().contains("legacy-test-secret"));
    stage(
        &mut config,
        RegistryEdit {
            expected: expected.clone(),
            registry: expected,
        },
    )
    .unwrap();
    assert_eq!(
        config.providers.connections["legacy.openai.legacy.prod"].api_key(),
        Some("legacy-test-secret")
    );
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["connections"]["legacy.openai.legacy.prod"]["uri"] = json!("https://updated.test/v1");
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    assert_eq!(
        config
            .provider_registry()
            .unwrap()
            .resolve_chat(&"openai.legacy.prod".into())
            .unwrap()
            .selection
            .connection
            .uri(),
        Some("https://updated.test/v1")
    );
    let encoded = toml::to_string(&config.providers).unwrap();
    let reloaded = Config {
        providers: toml::from_str(&encoded).unwrap(),
        ..Config::default()
    };
    assert_eq!(snapshot(&reloaded).unwrap(), snapshot(&config).unwrap());
}

#[test]
fn registry_voice_pickers_use_complete_model_refs_and_correct_capabilities() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    for (name, capability) in [
        ("voice.en.us", "tts"),
        ("asr.en.us", "asr"),
        ("embed.docs.v1", "embedding"),
        ("decision.fast.v1", "decision"),
    ] {
        registry["models"][name] =
            json!({"capability": capability, "provider": "gateway.prod", "model": "upstream"});
    }
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    assert_eq!(
        config.resolve_alias_source(zeroclaw_config::traits::AliasSource::TtsProviders),
        vec!["voice.en.us"]
    );
    assert_eq!(
        config.resolve_alias_source(zeroclaw_config::traits::AliasSource::TranscriptionProviders),
        vec!["asr.en.us"]
    );
    let chat = config.resolve_alias_source(zeroclaw_config::traits::AliasSource::ModelProviders);
    assert!(chat.contains(&"chat.fast.us".to_owned()));
    assert!(!chat.contains(&"voice.en.us".to_owned()));
    assert!(!chat.contains(&"embed.docs.v1".to_owned()));
}

#[test]
fn registry_editor_rejects_stale_or_invalid_replacements_atomically() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    let before = serde_json::to_value(&config.providers).unwrap();
    let mut invalid = expected.clone();
    invalid["fallback"]["chat.fast.us"] = json!(["missing.model"]);
    assert!(
        stage(
            &mut config,
            RegistryEdit {
                expected: expected.clone(),
                registry: invalid
            }
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
    let mut stale = expected.clone();
    stale["models"]["chat.fast.us"]["model"] = json!("different");
    assert!(
        stage(
            &mut config,
            RegistryEdit {
                expected: stale,
                registry: expected
            }
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&config.providers).unwrap(), before);
}

#[test]
fn registry_editor_rejects_orphan_mask_but_accepts_explicit_secret_replacement() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["connections"]["new.connection"] = registry["connections"]["gateway.prod"].clone();
    assert!(
        stage(
            &mut config,
            RegistryEdit {
                expected: expected.clone(),
                registry
            }
        )
        .is_err()
    );
    let mut registry = expected.clone();
    registry["connections"]["gateway.prod"]["auth"]["api_key"] = json!("replacement-secret");
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    assert_eq!(
        serde_json::to_value(&config.providers).unwrap()["connections"]["gateway.prod"]["auth"]["api_key"],
        "replacement-secret"
    );
}

#[test]
fn registry_editor_retains_models_referenced_by_memory() {
    let mut config = config();
    let expected = snapshot(&config).unwrap();
    let mut registry = expected.clone();
    registry["models"]["embed.active"] =
        json!({"capability": "embedding", "provider": "gateway.prod", "model": "embedding-v1"});
    stage(&mut config, RegistryEdit { expected, registry }).unwrap();
    config.memory.embedding_model_ref = Some("embed.active".into());
    let expected = snapshot(&config).unwrap();
    for replacement in [
        None,
        Some(json!({"capability": "chat", "provider": "gateway.prod", "model": "chat-v1"})),
    ] {
        let mut registry = expected.clone();
        if let Some(replacement) = replacement {
            registry["models"]["embed.active"] = replacement;
        } else {
            registry["models"]
                .as_object_mut()
                .unwrap()
                .remove("embed.active");
        }
        let error = stage(
            &mut config,
            RegistryEdit {
                expected: expected.clone(),
                registry,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("memory.embedding_model_ref"));
        assert_eq!(snapshot(&config).unwrap(), expected);
    }
}

#[test]
fn registry_connection_header_secrets_follow_encryption_and_decryption() {
    use zeroclaw_config::secrets::SecretStore;
    let tmp = tempfile::tempdir().unwrap();
    let store = SecretStore::new(tmp.path(), true);
    let mut config = config();
    let connection = config
        .providers
        .connections
        .get_mut("gateway.prod")
        .unwrap();
    let original = serde_json::to_value(&*connection).unwrap();
    connection
        .encrypt_secrets(&store, "providers.connections.gateway.prod")
        .unwrap();
    let encrypted = serde_json::to_value(&*connection).unwrap();
    for value in encrypted["extra_headers"].as_object().unwrap().values() {
        assert!(SecretStore::is_encrypted(value.as_str().unwrap()));
    }
    assert!(SecretStore::is_encrypted(
        encrypted["auth"]["api_key"].as_str().unwrap()
    ));
    connection
        .decrypt_secrets(&store, "providers.connections.gateway.prod")
        .unwrap();
    assert_eq!(serde_json::to_value(connection).unwrap(), original);
}

#[test]
fn exact_model_rename_rewrites_consumers_and_fallback_without_prefix_matching() {
    use zeroclaw_config::provider_registry::editor::{create_entry, delete_entry, rename_entry};
    use zeroclaw_config::schema::{AliasedAgentConfig, ModelRouteConfig};
    let mut config = config();
    create_entry(
        &mut config,
        "models",
        "chat.fast.us.extra",
        json!({"capability":"chat", "provider":"gateway.prod", "model":"model-one"}),
    )
    .unwrap();
    config.agents.insert(
        "consumer".into(),
        AliasedAgentConfig {
            model_provider: "chat.fast.us".into(),
            classifier_provider: "chat.fast.us".into(),
            summary_provider: "chat.fast.us".into(),
            ..Default::default()
        },
    );
    config.model_routes.push(ModelRouteConfig {
        hint: "fast".into(),
        model_ref: "chat.fast.us".into(),
    });
    config.multimodal.vision_model_provider = Some("chat.fast.us".into());
    rename_entry(&mut config, "models", "chat.fast.us", "chat.Next.v2").unwrap();
    let registry = config.provider_registry().unwrap();
    assert!(!registry.models.contains_key("chat.fast.us"));
    assert!(registry.models.contains_key("chat.fast.us.extra"));
    assert_eq!(
        registry.fallback["chat.Next.v2"],
        vec!["chat.backup".into()]
    );
    assert_eq!(
        config.agents["consumer"].model_provider.as_str(),
        "chat.Next.v2"
    );
    assert_eq!(
        config.agents["consumer"].classifier_provider.as_str(),
        "chat.Next.v2"
    );
    assert_eq!(
        config.agents["consumer"].summary_provider.as_str(),
        "chat.Next.v2"
    );
    assert_eq!(config.model_routes[0].model_ref, "chat.Next.v2");
    assert_eq!(
        config.multimodal.vision_model_provider.as_deref(),
        Some("chat.Next.v2")
    );
    let before = snapshot(&config).unwrap();
    assert!(delete_entry(&mut config, "models", "chat.Next.v2").is_err());
    assert_eq!(snapshot(&config).unwrap(), before);
}

#[test]
fn exact_connection_rename_preserves_credentials_and_rebuilds_bindings() {
    use zeroclaw_config::provider_registry::editor::rename_entry;
    let mut config = config();
    rename_entry(
        &mut config,
        "connections",
        "gateway.prod",
        "gateway.Next.v2",
    )
    .unwrap();
    let registry = config.provider_registry().unwrap();
    let model = registry.resolve_chat(&"chat.fast.us".into()).unwrap();
    assert_eq!(model.selection.model.provider().as_str(), "gateway.Next.v2");
    assert_eq!(model.selection.connection.api_key(), Some("test-secret"));
    assert_eq!(
        model.selection.connection.extra_headers().unwrap()["X-Custom-Auth"],
        "custom-header-secret"
    );
    assert!(!registry.connections.contains_key("gateway.prod"));
}

#[test]
fn preset_rename_updates_api_and_model_bindings() {
    use zeroclaw_config::provider_registry::editor::rename_entry;
    let mut config = Config {
        providers: serde_json::from_value(json!({
            "connections":{"gateway.prod":{"kind":"custom", "uri":"https://example.test/v1", "api":{"chat":{"preset":"common.v1"}}}},
            "model_presets":{"common.v1":{"capability":"chat", "options":{"temperature":0.4}}},
            "models":{"chat.Fast":{"capability":"chat", "provider":"gateway.prod", "model":"wire-model", "preset":"common.v1"}}
        })).unwrap(), ..Config::default()
    };
    rename_entry(&mut config, "model_presets", "common.v1", "common.v2").unwrap();
    let registry = snapshot(&config).unwrap();
    assert_eq!(
        registry["connections"]["gateway.prod"]["api"]["chat"]["preset"],
        "common.v2"
    );
    assert_eq!(registry["models"]["chat.Fast"]["preset"], "common.v2");
}

#[test]
fn registry_rename_refuses_environment_owned_consumer_bindings_atomically() {
    let mut config = config();
    config
        .agents
        .insert("test-agent".into(), Default::default());
    config.agents.get_mut("test-agent").unwrap().model_provider = "chat.fast.us".into();
    config
        .env_overridden_paths
        .insert("agents.test-agent.model_provider".into());
    let before = serde_json::to_value(&config).unwrap();
    let error = zeroclaw_config::provider_registry::editor::rename_entry(
        &mut config,
        "models",
        "chat.fast.us",
        "chat.next",
    )
    .unwrap_err();
    assert!(error.to_string().contains("environment-owned reference"));
    assert_eq!(serde_json::to_value(&config).unwrap(), before);
}
