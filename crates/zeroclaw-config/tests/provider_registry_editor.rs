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
