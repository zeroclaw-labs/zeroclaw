use zeroclaw_api::capability::Capability;
use zeroclaw_config::{migration, schema::Config};

fn legacy(version: u32) -> String {
    format!(
        r#"schema_version = {version}
[providers.models.openai.shared]
model = "chat-upstream"
api_key = "chat-fixture-key"
[providers.tts.openai.shared]
model = "tts-1"
voice = "alloy"
api_key = "tts-fixture-key"
[providers.transcription.openai.shared]
model = "whisper-1"
api_key = "asr-fixture-key"
[agents.worker]
model_provider = "openai.shared"
tts_provider = "openai.shared"
transcription_provider = "openai.shared"
[memory]
embedding_provider = "openai.shared"
embedding_model = "embedding-upstream"
embedding_dimensions = 384
[[model_routes]]
hint = "fast"
model_provider = "openai.shared"
model = "route-upstream"
"#
    )
}

#[test]
fn loading_retires_provider_input_before_any_consumer_sees_config() {
    for version in [3, 4] {
        let config = migration::migrate_to_current(&legacy(version)).unwrap();
        assert!(config.providers.models.iter_entries().next().is_none());
        assert!(config.providers.tts.is_empty());
        assert!(config.providers.transcription.is_empty());
        let registry = config.provider_registry().unwrap();
        let agent = config.agent("worker").unwrap();
        for (reference, capability, key) in [
            (
                agent.model_provider.as_str(),
                Capability::Chat,
                "chat-fixture-key",
            ),
            (
                agent.tts_provider.as_str(),
                Capability::Tts,
                "tts-fixture-key",
            ),
            (
                agent.transcription_provider.as_str(),
                Capability::Asr,
                "asr-fixture-key",
            ),
        ] {
            assert_eq!(
                registry
                    .selection(&reference.into(), capability)
                    .unwrap()
                    .connection
                    .api_key(),
                Some(key)
            );
        }
        let embedding_ref = config.memory.embedding_model_ref.as_deref().unwrap();
        let embedding = registry.resolve_embedding(&embedding_ref.into()).unwrap();
        assert_eq!(embedding.model, "embedding-upstream");
        assert_eq!(embedding.dimensions, Some(384));
        assert_eq!(
            config
                .resolve_model_id(&config.model_routes[0].model_ref)
                .as_deref(),
            Some("route-upstream")
        );
        let round_trip: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&round_trip.providers).unwrap(),
            serde_json::to_value(&config.providers).unwrap()
        );
    }
}

#[test]
fn disk_migration_and_loading_produce_the_same_clean_input() {
    for version in [3, 4] {
        let raw = legacy(version);
        let expected = migration::migrate_to_current(&raw).unwrap();
        let (disk, _) = migration::migrate_file_with_notices(&raw).unwrap().unwrap();
        let reloaded = migration::migrate_to_current(&disk).unwrap();
        assert_eq!(
            serde_json::to_value(&reloaded.providers).unwrap(),
            serde_json::to_value(&expected.providers).unwrap()
        );
        assert_eq!(
            reloaded.agent("worker").unwrap().tts_provider,
            expected.agent("worker").unwrap().tts_provider
        );
        let document: toml::Value = toml::from_str(&disk).unwrap();
        assert!(document["providers"].get("tts").is_none());
        assert!(document["providers"].get("transcription").is_none());
        assert!(document["providers"]["models"].get("openai").is_none());
        assert!(
            migration::migrate_file_with_notices(&disk)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn direct_deserialization_is_also_an_input_boundary() {
    let config: Config = toml::from_str(
        r#"
[providers.models.gemini.work]
model="gemini-test"
oauth_client_id="test-client"
oauth_client_secret="test-secret"
"#,
    )
    .unwrap();
    assert!(config.providers.models.gemini.is_empty());
    assert_eq!(
        config
            .gemini_oauth_client_credentials("gemini.work")
            .unwrap(),
        Some(("test-client".into(), "test-secret".into()))
    );
}

#[test]
fn registry_getter_never_projects_manually_injected_retired_tables() {
    let mut config = Config::default();
    config
        .providers
        .models
        .ensure("openai", "unadmitted")
        .unwrap()
        .model = Some("legacy-upstream".into());
    assert!(
        !config
            .provider_registry()
            .unwrap()
            .models
            .contains_key("openai.unadmitted")
    );
}

#[test]
fn encrypted_credentials_survive_relocation_and_reload() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
    let encrypted = store.encrypt("fixture-secret").unwrap();
    let raw = format!(
        r#"schema_version = 3
[providers.models.openai.work]
model="chat-model"
api_key="{encrypted}"
"#
    );
    let mut config = migration::migrate_to_current(&raw).unwrap();
    assert_eq!(
        config.providers.connections["legacy.openai.work"].api_key(),
        Some(encrypted.as_str())
    );
    config.decrypt_secrets(&store).unwrap();
    assert_eq!(
        config.providers.connections["legacy.openai.work"].api_key(),
        Some("fixture-secret")
    );
    config.encrypt_secrets(&store).unwrap();
    let written = toml::to_string(&config).unwrap();
    assert!(!written.contains("fixture-secret"));
    let mut reloaded = migration::migrate_to_current(&written).unwrap();
    reloaded.decrypt_secrets(&store).unwrap();
    assert_eq!(
        reloaded.providers.connections["legacy.openai.work"].api_key(),
        Some("fixture-secret")
    );
    assert!(reloaded.providers.models.openai.is_empty());
}

#[test]
fn mixed_inputs_validate_fallback_after_legacy_models_are_normalized() {
    let config: Config = toml::from_str(
        r#"
[providers.connections.gateway]
kind = "openai"
[providers.models.primary]
capability = "chat"
provider = "gateway"
model = "primary-model"
[providers.models.openai.backup]
model = "backup-model"
[providers.fallback]
primary = ["openai.backup"]
"#,
    )
    .unwrap();
    assert!(config.providers.models.openai.is_empty());
    let registry = config.provider_registry().unwrap();
    assert_eq!(
        registry
            .resolve_chat(&"openai.backup".into())
            .unwrap()
            .upstream_model(),
        Some("backup-model")
    );
    assert_eq!(registry.fallback["primary"][0].as_str(), "openai.backup");
}
