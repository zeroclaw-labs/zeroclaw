use zeroclaw_config::schema::Config;

#[tokio::test]
async fn canonical_plan_alias_creation_setter_and_dirty_save_reload() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config {
        config_path: root.path().join("config.toml"),
        ..Default::default()
    };
    assert!(
        zeroclaw_config::alias_refs::create_map_key_checked(
            &mut config,
            "providers.models.openai",
            "subscriber"
        )
        .unwrap()
    );
    assert!(
        config.providers.models.openai["subscriber"]
            .base
            .chatgpt_plan_auth
            .is_none()
    );
    config
        .set_prop("providers.models.openai.subscriber.kind", "chatgpt-plan")
        .unwrap();
    config
        .set_prop("providers.models.openai.subscriber.model", "model-fixture")
        .unwrap();
    config
        .set_prop("providers.models.openai.subscriber.wire_api", "responses")
        .unwrap();
    config
        .set_prop(
            "providers.models.openai.subscriber.chatgpt_plan_auth.registration",
            "chatgpt-plan:subscriber",
        )
        .unwrap();
    let entry = config
        .providers
        .models
        .find("openai", "subscriber")
        .unwrap();
    assert_eq!(
        entry.chatgpt_plan_auth.as_ref().unwrap().registration,
        "chatgpt-plan:subscriber"
    );
    assert!(!entry.requires_openai_auth);
    assert!(entry.api_key.is_none());
    config.mark_dirty("providers.models.openai");
    config.save_dirty().await.unwrap();
    let raw = std::fs::read_to_string(&config.config_path).unwrap();
    assert!(raw.contains("chatgpt-plan:subscriber"));
    assert!(!root.path().join("auth-profiles.json").exists());
    let mut restored: Config = toml::from_str(&raw).unwrap();
    restored.config_path = config.config_path.clone();
    // The persistent setter must also resolve the nested map-key property.
    restored
        .set_prop_persistent(
            "providers.models.openai.subscriber.chatgpt_plan_auth.registration",
            "chatgpt-plan:reauthorized",
        )
        .unwrap();
    restored.save_dirty().await.unwrap();
    let saved = std::fs::read_to_string(&restored.config_path).unwrap();
    let restored: Config = toml::from_str(&saved).unwrap();
    let entry = restored
        .providers
        .models
        .find("openai", "subscriber")
        .unwrap();
    assert_eq!(
        entry.chatgpt_plan_auth.as_ref().unwrap().registration,
        "chatgpt-plan:reauthorized"
    );
    assert_eq!(entry.kind.as_deref(), Some("chatgpt-plan"));
    assert_eq!(entry.model.as_deref(), Some("model-fixture"));
    for secret in [
        "access_token",
        "refresh_token",
        "id_token",
        "client_id",
        "subject",
    ] {
        assert!(!saved.contains(secret));
    }
}

#[test]
fn canonical_generic_alias_creation_does_not_gain_plan_or_codex_auth() {
    let mut config = Config::default();
    assert!(
        zeroclaw_config::alias_refs::create_map_key_checked(
            &mut config,
            "providers.models.openai",
            "ordinary"
        )
        .unwrap()
    );
    config
        .set_prop("providers.models.openai.ordinary.model", "model-fixture")
        .unwrap();
    let entry = config.providers.models.find("openai", "ordinary").unwrap();
    assert!(entry.chatgpt_plan_auth.is_none());
    assert!(!entry.requires_openai_auth);
    assert!(entry.api_key.is_none());
    assert!(entry.kind.is_none());
}

#[test]
fn chatgpt_plan_config_reference_roundtrips_without_credentials_or_legacy_auth() {
    let raw = r#"
[providers.models.openai.subscriber]
kind = "chatgpt-plan"
model = "model-fixture"
wire_api = "responses"
[providers.models.openai.subscriber.chatgpt_plan_auth]
registration = "chatgpt-plan:subscriber"
"#;
    let config: Config = toml::from_str(raw).unwrap();
    let entry = config
        .providers
        .models
        .find("openai", "subscriber")
        .unwrap();
    assert_eq!(
        entry.chatgpt_plan_auth.as_ref().unwrap().registration,
        "chatgpt-plan:subscriber"
    );
    assert!(!entry.requires_openai_auth);
    assert!(entry.api_key.is_none());
    let saved = toml::to_string(&config).unwrap();
    let restored: Config = toml::from_str(&saved).unwrap();
    assert_eq!(
        restored
            .providers
            .models
            .find("openai", "subscriber")
            .unwrap()
            .chatgpt_plan_auth
            .as_ref()
            .unwrap()
            .registration,
        "chatgpt-plan:subscriber"
    );
    for key in [
        "access_token",
        "refresh_token",
        "id_token",
        "subject",
        "client_id",
    ] {
        assert!(!saved.contains(key));
    }
}

#[test]
fn legacy_codex_config_keeps_its_existing_auth_semantics() {
    let config: Config = toml::from_str(
        r#"
[providers.models.openai.codex]
requires_openai_auth = true
wire_api = "responses"
"#,
    )
    .unwrap();
    let entry = config.providers.models.find("openai", "codex").unwrap();
    assert!(entry.requires_openai_auth);
    assert!(entry.chatgpt_plan_auth.is_none());
}
