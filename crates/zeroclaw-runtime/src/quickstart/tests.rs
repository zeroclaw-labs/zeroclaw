#[cfg(test)]
use super::*;
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice,
};
use zeroclaw_config::schema::Config;

#[test]
fn channel_type_options_cover_every_retained_schema_channel() {
    let cfg = Config::default();
    let picker = build_channel_type_options(&cfg.channels);
    let retained: Vec<_> = cfg
        .channels
        .channels()
        .into_iter()
        .filter(|info| !info.retired)
        .collect();
    assert_eq!(
        picker.len(),
        retained.len(),
        "Quickstart channel-type picker count diverged from the retained \
         (non-retired) ChannelsConfig::channels() rows; picker has {} rows, \
         retained schema rows: {}",
        picker.len(),
        retained.len(),
    );
    for (picked, expected) in picker.iter().zip(retained.iter()) {
        assert_eq!(
            picked.kind, expected.kind,
            "kind mismatch at {} — picker `{}`, schema `{}`",
            picked.display_name, picked.kind, expected.kind,
        );
        assert_eq!(
            picked.display_name, expected.name,
            "display_name mismatch at `{}` — picker `{}`, schema `{}`",
            picked.kind, picked.display_name, expected.name,
        );
    }
    // Retired transports must never reach the first-run picker.
    assert!(
        picker
            .iter()
            .all(|p| p.kind != "slack" && p.kind != "matrix"),
        "retired transports leaked into the quickstart picker"
    );
}

#[test]
fn provider_runtime_defaults_follow_canonical_provider_recommendations() {
    let snapshot = snapshot_state(&Config::default());

    let local = snapshot
        .model_provider_types
        .iter()
        .find(|provider| provider.kind == "lmstudio")
        .expect("LM Studio should be present in the canonical provider registry");
    assert!(local.local);
    assert_eq!(
        local.default_runtime_profile.as_deref(),
        Some("local_small")
    );

    let ollama = snapshot
        .model_provider_types
        .iter()
        .find(|provider| provider.kind == "ollama")
        .expect("Ollama should be present in the canonical provider registry");
    assert!(ollama.local);
    assert_eq!(
        ollama.default_runtime_profile.as_deref(),
        None,
        "providers without native tools must use the canonical fallback",
    );

    let remote = snapshot
        .model_provider_types
        .iter()
        .find(|provider| provider.kind == "anthropic")
        .expect("Anthropic should be present in the canonical provider registry");
    assert!(!remote.local);
    assert_eq!(remote.default_runtime_profile, None);
    assert_eq!(snapshot.default_runtime_profile, "unbounded");

    for provider in &snapshot.model_provider_types {
        if let Some(default) = provider.default_runtime_profile.as_deref() {
            assert!(
                snapshot
                    .runtime_presets
                    .iter()
                    .any(|preset| preset.preset_name == default),
                "provider {} advertised unavailable runtime preset {default}",
                provider.kind,
            );
        }
    }
}

fn fresh_submission(agent_name: &str) -> BuilderSubmission {
    BuilderSubmission {
        model_provider: SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "anthropic".into(),
            alias: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::from([(
                "api_key".to_string(),
                "sk-test".to_string(),
            )]),
        }),
        risk_profile: SelectorChoice::Fresh("balanced".into()),
        runtime_profile: SelectorChoice::Fresh("balanced".into()),
        memory: SelectorChoice::Fresh(if zeroclaw_memory::TACHI_COMPILED {
            MemoryChoice::Tachi
        } else {
            MemoryChoice::Markdown
        }),
        channels: vec![],
        peer_groups: vec![],
        agent: AgentIdentity {
            name: agent_name.into(),
            system_prompt: "You are helpful.".into(),
            personality_file: None,
            personality_files: vec![],
        },
    }
}

fn fresh_channel(channel_type: &str, alias: &str, fields: &[(&str, &str)]) -> ChannelQuickStart {
    ChannelQuickStart {
        channel_type: channel_type.into(),
        alias: alias.into(),
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    }
}

fn apply_fresh_provider(
    choice: ModelProviderChoice,
) -> (Config, Option<AppliedAgent>, Vec<QuickstartError>) {
    let mut cfg = Config::default();
    let mut submission = fresh_submission("bot");
    submission.model_provider = SelectorChoice::Fresh(choice);
    let mut staged = Vec::new();
    let mut errors = Vec::new();
    let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
    (cfg, applied, errors)
}

#[test]
fn existing_markdown_memory_storage_ref_is_accepted() {
    let mut cfg = Config::default();
    cfg.storage.markdown.insert(
        "default".into(),
        zeroclaw_config::schema::MarkdownStorageConfig::default(),
    );
    let choice = SelectorChoice::Existing("markdown.default".to_string());
    let mut errors = Vec::new();

    let applied = apply_memory(&mut cfg, &choice, &mut errors, None);

    assert!(errors.is_empty(), "apply_memory errors: {errors:?}");
    assert_eq!(applied.as_deref(), Some("markdown.default"));
    assert_eq!(cfg.memory.backend, "markdown.default");
}

#[test]
fn memory_storage_refs_from_snapshot_are_accepted() {
    let mut cfg = Config::default();
    cfg.storage.markdown.insert(
        "default".into(),
        zeroclaw_config::schema::MarkdownStorageConfig::default(),
    );
    let snapshot = snapshot_state(&cfg);

    assert!(
        snapshot
            .storage
            .iter()
            .any(|ref_| ref_ == "markdown.default"),
        "snapshot should expose configured markdown storage: {:?}",
        snapshot.storage
    );
    for reference in snapshot.storage {
        let mut candidate = cfg.clone();
        let choice = SelectorChoice::Existing(reference.clone());
        let mut errors = Vec::new();

        let applied = apply_memory(&mut candidate, &choice, &mut errors, None);

        assert!(
            errors.is_empty(),
            "snapshot storage ref {reference:?} should apply without errors: {errors:?}"
        );
        assert_eq!(applied.as_deref(), Some(reference.as_str()));
        assert_eq!(candidate.memory.backend, reference);
    }
}

#[test]
fn quickstart_fresh_tachi_memory_uses_bare_backend_and_gates_on_feature() {
    let mut cfg = Config::default();
    let snapshot = snapshot_state(&cfg);
    assert_eq!(
        snapshot.memory_kinds.iter().any(|kind| kind == "tachi"),
        zeroclaw_memory::TACHI_COMPILED
    );

    let choice = SelectorChoice::Fresh(MemoryChoice::Tachi);
    let mut errors = Vec::new();
    let applied = apply_memory(&mut cfg, &choice, &mut errors, None);

    if zeroclaw_memory::TACHI_COMPILED {
        assert!(errors.is_empty(), "apply_memory errors: {errors:?}");
        assert_eq!(applied.as_deref(), Some("tachi"));
        assert_eq!(cfg.memory.backend, "tachi");
        assert!(
            cfg.validate().is_ok(),
            "Config with bare tachi memory backend must validate"
        );

        let mut builder_cfg = Config::default();
        let mut submission = fresh_submission("tachi_bot");
        submission.memory = SelectorChoice::Fresh(MemoryChoice::Tachi);
        let mut staged = Vec::new();
        let mut submit_errors = Vec::new();
        let applied_agent = apply_into(
            &mut builder_cfg,
            &submission,
            &mut staged,
            &mut submit_errors,
            None,
        );
        assert!(
            submit_errors.is_empty(),
            "apply_into errors: {submit_errors:?}"
        );
        assert!(applied_agent.is_some());
        assert_eq!(builder_cfg.memory.backend, "tachi");
    } else {
        assert!(applied.is_none());
        assert!(
            errors
                .iter()
                .any(|err| err.message.contains("memory-tachi")),
            "expected feature-gate QuickstartError, got {errors:?}"
        );
    }
}

#[test]
fn apply_serializes_provider_fields_as_snake_case() {
    let mut cfg = Config::default();
    let submission = fresh_submission("bot");
    let mut staged = Vec::new();
    let mut errors = Vec::new();
    let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some(), "apply_into should yield an agent");
    // The submission carries the snake field key `api_key` and it must
    // land on disk as the snake serde field `api_key`, never kebab.
    let toml = toml::to_string(&cfg).expect("serialize config");
    assert!(
        toml.contains("api_key"),
        "expected snake `api_key` in serialized config:\n{toml}"
    );
    assert!(
        !toml.contains("api-key"),
        "kebab `api-key` leaked into serialized config:\n{toml}"
    );
}

#[test]
fn apply_provider_type_trims_and_canonicalizes_whitespace() {
    // A provider type with stray whitespace must canonicalize to the
    // registry's family key, not reach create_map_key verbatim (which would
    // fail with "no map-keyed/list section at providers.models.llamacpp ").
    let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "  llamacpp  ".into(),
        alias: "local".into(),
        model: "qwen2.5-coder".into(),
        fields: std::collections::HashMap::new(),
    });
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    assert!(
        cfg.providers.models.find("llamacpp", "local").is_some(),
        "expected providers.models.llamacpp.local to exist"
    );
    let agent = cfg.agents.get("bot").expect("agent created");
    assert_eq!(agent.model_provider.as_str(), "llamacpp.local");
}

#[test]
fn apply_provider_type_case_insensitive() {
    let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "Anthropic".into(),
        alias: "main".into(),
        model: "claude-sonnet-4-5".into(),
        fields: std::collections::HashMap::new(),
    });
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    assert!(cfg.providers.models.find("anthropic", "main").is_some());
}

#[test]
fn apply_claude_alias_writes_canonical_anthropic_config() {
    let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "claude".into(),
        alias: "max".into(),
        model: "claude-sonnet-4-5".into(),
        fields: std::collections::HashMap::from([
            ("auth_mode".to_string(), "setup_token".to_string()),
            ("api_key".to_string(), "sk-ant-oat01-test-token".to_string()),
        ]),
    });
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    let entry = cfg
        .providers
        .models
        .find("anthropic", "max")
        .expect("anthropic.max entry");
    assert_eq!(entry.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(entry.api_key.as_deref(), Some("sk-ant-oat01-test-token"));
    assert!(
        cfg.get_prop("providers.models.anthropic.max.auth_mode")
            .is_err()
    );
    let agent = cfg.agents.get("bot").expect("agent created");
    assert_eq!(agent.model_provider.as_str(), "anthropic.max");
}

#[test]
fn apply_openai_codex_alias_writes_canonical_openai_auth_config() {
    let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "openai-codex".into(),
        alias: "coding".into(),
        model: "gpt-5.4".into(),
        fields: std::collections::HashMap::new(),
    });
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    let entry = cfg
        .providers
        .models
        .find("openai", "coding")
        .expect("openai.coding entry");
    assert_eq!(entry.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(entry.wire_api, Some(WireApi::Responses));
    assert!(entry.requires_openai_auth);
    let agent = cfg.agents.get("bot").expect("agent created");
    assert_eq!(agent.model_provider.as_str(), "openai.coding");
}

#[test]
fn apply_openai_auth_mode_codex_ignores_api_key_field() {
    let (cfg, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "openai".into(),
        alias: "coding".into(),
        model: "gpt-5.4".into(),
        fields: std::collections::HashMap::from([
            ("auth_mode".to_string(), "codex".to_string()),
            ("api_key".to_string(), "sk-should-not-persist".to_string()),
        ]),
    });
    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    let entry = cfg
        .providers
        .models
        .find("openai", "coding")
        .expect("openai.coding entry");
    assert_eq!(entry.wire_api, Some(WireApi::Responses));
    assert!(entry.requires_openai_auth);
    assert!(
        entry.api_key.is_none(),
        "Codex auth must not persist an API key from the Quickstart form"
    );
}

#[test]
fn apply_unknown_anthropic_auth_mode_errors_clearly() {
    let (_, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "anthropic".into(),
        alias: "main".into(),
        model: "claude-sonnet-4-5".into(),
        fields: std::collections::HashMap::from([(
            "auth_mode".to_string(),
            "not_real".to_string(),
        )]),
    });
    assert!(applied.is_none());
    assert!(
        errors
            .iter()
            .any(|e| e.step == QuickstartStep::ModelProvider
                && e.field == "auth_mode"
                && e.message.contains("unknown Anthropic auth mode")),
        "expected a clear unknown-Anthropic-auth-mode error, got: {errors:?}"
    );
}

#[test]
fn apply_unknown_provider_type_errors_clearly() {
    let (_, applied, errors) = apply_fresh_provider(ModelProviderChoice {
        provider_type: "not_a_real_provider".into(),
        alias: "x".into(),
        model: "m".into(),
        fields: std::collections::HashMap::new(),
    });
    assert!(applied.is_none());
    assert!(
        errors
            .iter()
            .any(|e| e.step == QuickstartStep::ModelProvider
                && e.message.contains("unknown model provider type")),
        "expected a clear unknown-provider error, got: {errors:?}"
    );
}

#[test]
fn validate_only_passes_on_fresh_submission() {
    let cfg = Config::default();
    let submission = fresh_submission("bot");
    validate_only(&submission, &cfg).expect("fresh submission validates");
}

#[test]
fn validate_only_rejects_blank_agent_name() {
    let cfg = Config::default();
    let submission = fresh_submission("");
    let errors = validate_only(&submission, &cfg).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.step == QuickstartStep::Agent && e.field == "name")
    );
}

#[test]
fn validate_only_rejects_existing_agent_name() {
    let mut cfg = Config::default();
    cfg.agents.insert(
        "bot".into(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
    );
    let submission = fresh_submission("bot");
    let errors = validate_only(&submission, &cfg).unwrap_err();
    assert!(errors.iter().any(|e| e.step == QuickstartStep::Agent));
}

#[test]
fn validate_only_rejects_unknown_risk_preset() {
    let cfg = Config::default();
    let mut submission = fresh_submission("bot");
    submission.risk_profile = SelectorChoice::Fresh("does-not-exist".into());
    let errors = validate_only(&submission, &cfg).unwrap_err();
    assert!(errors.iter().any(|e| e.step == QuickstartStep::RiskProfile));
}

#[tokio::test]
async fn rejected_runtime_selection_leaves_live_config_unchanged() {
    let mut cfg = Config::default();
    let before = serde_json::to_value(&cfg).expect("serialize initial config");
    let before_dirty_paths = cfg.dirty_paths.clone();
    let mut submission = fresh_submission("bot");
    submission.runtime_profile = SelectorChoice::Fresh("does-not-exist".into());

    let errors = apply(submission, &mut cfg).await.unwrap_err();

    assert!(
        errors
            .iter()
            .any(|error| error.step == QuickstartStep::RuntimeProfile),
        "expected a runtime-profile error, got {errors:?}",
    );
    assert_eq!(
        serde_json::to_value(&cfg).expect("serialize rejected config"),
        before,
    );
    assert_eq!(cfg.dirty_paths, before_dirty_paths);
}

#[tokio::test]
async fn missing_existing_runtime_leaves_live_config_unchanged() {
    let mut cfg = Config::default();
    let before = serde_json::to_value(&cfg).expect("serialize initial config");
    let before_dirty_paths = cfg.dirty_paths.clone();
    let mut submission = fresh_submission("bot");
    submission.runtime_profile = SelectorChoice::Existing("missing".into());

    let errors = apply(submission, &mut cfg).await.unwrap_err();

    assert!(
        errors
            .iter()
            .any(|error| error.step == QuickstartStep::RuntimeProfile),
        "expected a runtime-profile error, got {errors:?}",
    );
    assert_eq!(
        serde_json::to_value(&cfg).expect("serialize rejected config"),
        before,
    );
    assert_eq!(cfg.dirty_paths, before_dirty_paths);
}

#[test]
fn validate_only_accepts_every_builtin_risk_preset() {
    let cfg = Config::default();
    for p in super::presets::RISK_PRESETS {
        let mut submission = fresh_submission("bot");
        submission.risk_profile = SelectorChoice::Fresh(p.preset_name.into());
        validate_only(&submission, &cfg)
            .unwrap_or_else(|e| panic!("risk preset `{}` failed validate: {e:?}", p.preset_name));
    }
}

#[test]
fn field_shape_returns_model_provider_rows_for_canonical_types() {
    for kind in ["anthropic", "openai", "ollama", "openrouter", "groq"] {
        let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(
            keys.contains(&"model"),
            "field_shape for `{kind}` is missing `model` row; got {keys:?}",
        );
        assert!(
            keys.contains(&"api_key"),
            "field_shape for `{kind}` is missing `api_key` row; got {keys:?}",
        );
    }
}

/// Codex subscription auth: `field_shape(ModelProvider, "openai")` exposes
/// one Quickstart-only auth selector instead of raw config toggles. Apply
/// translates `auth_mode = "codex"` into the canonical persisted
/// `wire_api = "responses"` + `requires_openai_auth = true` fields.
#[test]
fn field_shape_openai_includes_codex_auth_mode() {
    let rows = super::field_shape(super::FieldSection::ModelProvider, "openai");
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert!(
        keys.contains(&"auth_mode"),
        "field_shape for openai must include `auth_mode` for Codex subscription; got {keys:?}",
    );
    assert!(
        !keys.contains(&"requires_openai_auth") && !keys.contains(&"wire_api"),
        "field_shape for openai should hide raw Codex config toggles; got {keys:?}",
    );
    let auth = rows
        .iter()
        .find(|row| row.key == "auth_mode")
        .expect("auth_mode row");
    assert!(auth.required);
    assert_eq!(
        auth.enum_variants.as_deref(),
        Some(["api_key".to_string(), "codex".to_string()].as_slice())
    );
    assert_eq!(auth.default.as_deref(), Some("api_key"));
    // No row may carry the `<unset>` placeholder as its default.
    // It's a display sentinel for an unset Option; echoing it back
    // through any surface (CLI/TUI/web) makes the daemon validate
    // `<unset>` against the field's real type and reject it.
    for row in &rows {
        assert_ne!(
            row.default.as_deref(),
            Some(zeroclaw_config::traits::UNSET_DISPLAY),
            "`{}` must not default to the <unset> placeholder",
            row.key
        );
    }
}

#[test]
fn field_shape_openai_codex_alias_preselects_codex_auth() {
    let rows = super::field_shape(super::FieldSection::ModelProvider, "openai-codex");
    let auth = rows
        .iter()
        .find(|row| row.key == "auth_mode")
        .expect("auth_mode row");
    assert_eq!(auth.default.as_deref(), Some("codex"));
}

#[test]
fn field_shape_anthropic_includes_claude_auth_mode() {
    let rows = super::field_shape(super::FieldSection::ModelProvider, "claude");
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert!(
        keys.contains(&"auth_mode"),
        "field_shape for claude/anthropic must include `auth_mode`; got {keys:?}",
    );
    let auth = rows
        .iter()
        .find(|row| row.key == "auth_mode")
        .expect("auth_mode row");
    assert!(auth.required);
    assert_eq!(
        auth.enum_variants.as_deref(),
        Some(["api_key".to_string(), "setup_token".to_string()].as_slice())
    );
    assert_eq!(auth.default.as_deref(), Some("api_key"));
    let api_key = rows
        .iter()
        .find(|row| row.key == "api_key")
        .expect("api_key row");
    assert!(
        api_key.help.contains("claude setup-token"),
        "Anthropic API key help should mention setup-token flow; got {:?}",
        api_key.help
    );
}

/// `api_key` must be non-required in the Quickstart form so Codex
/// subscription (no API key) and local providers (Ollama) can proceed
/// without one.
#[test]
fn field_shape_api_key_is_not_required() {
    for kind in ["openai", "ollama"] {
        let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
        let api_key_row = rows.iter().find(|r| r.key == "api_key");
        assert!(
            api_key_row.is_some(),
            "field_shape for `{kind}` must include `api_key`",
        );
        assert!(
            !api_key_row.unwrap().required,
            "`api_key` must be non-required for `{kind}` (Codex subscription / local providers don't need one)",
        );
    }
}

async fn apply_to_temp(submission: BuilderSubmission) -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        config_path: dir.path().join("config.toml"),
        data_dir: dir.path().join("data"),
        ..Default::default()
    };
    config.save().await.unwrap();
    let mut config = config;
    super::apply(submission, &mut config)
        .await
        .expect("apply should succeed");
    (dir, config)
}

fn reload(dir: &tempfile::TempDir) -> Config {
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    toml::from_str(&raw).expect("on-disk config must round-trip")
}

#[tokio::test]
async fn fresh_preset_profiles_persist_to_disk() {
    let (dir, applied) = apply_to_temp(fresh_submission("bot")).await;
    assert!(applied.risk_profiles.contains_key("balanced"));
    assert!(applied.runtime_profiles.contains_key("balanced"));
    let reloaded = reload(&dir);
    assert!(
        reloaded.risk_profiles.contains_key("balanced"),
        "risk_profiles.balanced must survive save_dirty + reload, not dangle"
    );
    assert!(
        reloaded.runtime_profiles.contains_key("balanced"),
        "runtime_profiles.balanced must survive save_dirty + reload, not dangle"
    );
    let agent = reloaded.agents.get("bot").expect("agent persisted");
    assert_eq!(agent.risk_profile, "balanced");
    assert_eq!(agent.runtime_profile, "balanced");
}

#[tokio::test]
async fn existing_runtime_profile_is_reused_without_writing_preset() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        config_path: dir.path().join("config.toml"),
        data_dir: dir.path().join("data"),
        ..Default::default()
    };
    config.runtime_profiles.insert(
        "small-laptop".into(),
        zeroclaw_config::schema::RuntimeProfileConfig {
            max_tool_iterations: 2,
            ..Default::default()
        },
    );
    config.save().await.unwrap();

    let mut submission = fresh_submission("bot");
    submission.runtime_profile = SelectorChoice::Existing("small-laptop".into());
    super::apply(submission, &mut config)
        .await
        .expect("apply should reuse existing runtime profile");

    let reloaded = reload(&dir);
    assert!(
        reloaded.runtime_profiles.contains_key("small-laptop"),
        "existing runtime profile must stay configured"
    );
    assert!(
        !reloaded.runtime_profiles.contains_key("balanced"),
        "existing runtime profile choice must not write the fresh preset"
    );
    let agent = reloaded.agents.get("bot").expect("agent persisted");
    assert_eq!(agent.runtime_profile, "small-laptop");
    assert_eq!(
        reloaded
            .runtime_profiles
            .get("small-laptop")
            .expect("existing profile persisted")
            .max_tool_iterations,
        2,
        "existing profile values must not be clobbered"
    );
}

#[tokio::test]
async fn multiple_channels_all_bind_to_agent() {
    let mut submission = fresh_submission("bot");
    submission.channels = vec![
        SelectorChoice::Fresh(fresh_channel("telegram", "tg", &[("bot_token", "tok-a")])),
        SelectorChoice::Fresh(fresh_channel("discord", "dc", &[("bot_token", "tok-b")])),
    ];
    let (dir, _applied) = apply_to_temp(submission).await;
    let reloaded = reload(&dir);
    let agent = reloaded.agents.get("bot").expect("agent persisted");
    let bound: Vec<String> = agent.channels.iter().map(|c| c.to_string()).collect();
    assert!(
        bound.iter().any(|c| c.contains("tg")),
        "first channel must stay bound; got {bound:?}"
    );
    assert!(
        bound.iter().any(|c| c.contains("dc")),
        "second channel must also be bound; got {bound:?}"
    );
    assert_eq!(bound.len(), 2, "both channels bound, not just the last");
    let store = zeroclaw_config::secrets::SecretStore::new(dir.path(), true);
    assert_eq!(
        store
            .decrypt(&reloaded.channels.telegram["tg"].bot_token)
            .unwrap(),
        "tok-a"
    );
    assert!(reloaded.channels.telegram["tg"].enabled);
    assert_eq!(
        store
            .decrypt(&reloaded.channels.discord["dc"].bot_token)
            .unwrap(),
        "tok-b"
    );
    assert!(reloaded.channels.discord["dc"].enabled);
}

#[tokio::test]
async fn telegram_channel_fields_persist_canonical_bot_token() {
    let mut submission = fresh_submission("bot");
    submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
        "telegram",
        "ops",
        &[("bot_token", " 123:ABC ")],
    ))];

    let (dir, _) = apply_to_temp(submission).await;
    let reloaded = reload(&dir);
    let store = zeroclaw_config::secrets::SecretStore::new(dir.path(), true);
    assert_eq!(
        store
            .decrypt(&reloaded.channels.telegram["ops"].bot_token)
            .unwrap(),
        "123:ABC"
    );
    assert!(reloaded.channels.telegram["ops"].enabled);
}

#[test]
fn telegram_channel_fields_reject_unusable_bot_token_values() {
    for value in [
        None,
        Some(""),
        Some("   "),
        Some(zeroclaw_config::traits::UNSET_DISPLAY),
    ] {
        let cfg = Config::default();
        let mut submission = fresh_submission("bot");
        let fields = value.map_or_else(Vec::new, |value| vec![("bot_token", value)]);
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "telegram", "ops", &fields,
        ))];

        let errors = validate_only(&submission, &cfg).expect_err("token must be rejected");
        assert!(errors.iter().any(|error| {
            error.step == QuickstartStep::Channels
                && error.field == "channels[0].fields.bot_token"
                && error.message.contains("required")
        }));
    }
}

#[test]
fn discord_channel_fields_reject_unusable_bot_token_values() {
    // Discord twin of telegram_channel_fields_reject_unusable_bot_token_values:
    // the quickstart arm calls DiscordConfig::validate_bot_token.
    for value in [
        None,
        Some(""),
        Some("   "),
        Some(zeroclaw_config::traits::UNSET_DISPLAY),
    ] {
        let cfg = Config::default();
        let mut submission = fresh_submission("bot");
        let fields = value.map_or_else(Vec::new, |value| vec![("bot_token", value)]);
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            "discord", "ops", &fields,
        ))];

        let errors = validate_only(&submission, &cfg).expect_err("token must be rejected");
        assert!(errors.iter().any(|error| {
            error.step == QuickstartStep::Channels
                && error.field == "channels[0].fields.bot_token"
                && error.message.contains("required")
        }));
    }
}

#[test]
fn channel_fields_reject_unknown_keys_without_exposing_values() {
    let mut cfg = Config::default();
    let before = serde_json::to_value(&cfg).expect("serialize config");
    let before_dirty_paths = cfg.dirty_paths.clone();
    let mut submission = fresh_submission("bot");
    submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
        "discord",
        "ops",
        &[("unknown_secret", "super-secret-value")],
    ))];
    let mut errors = Vec::new();

    let refs = apply_channels(&mut cfg, &submission.channels, &mut errors, None);

    assert!(refs.is_empty());
    let error = errors
        .iter()
        .find(|error| error.field == "channels[0].fields.unknown_secret")
        .expect("structured unknown-field error");
    assert!(!error.message.contains("super-secret-value"));
    assert_eq!(
        serde_json::to_value(&cfg).expect("serialize config"),
        before
    );
    assert_eq!(cfg.dirty_paths, before_dirty_paths);
}

#[test]
fn channel_fields_reject_valid_but_unadvertised_schema_keys() {
    let mut cfg = Config::default();
    let before = serde_json::to_value(&cfg).expect("serialize config");
    let mut submission = fresh_submission("bot");
    submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
        "telegram",
        "ops",
        &[
            ("bot_token", "123:ABC"),
            ("api_base_url", "https://example.invalid"),
        ],
    ))];
    let mut errors = Vec::new();

    let refs = apply_channels(&mut cfg, &submission.channels, &mut errors, None);

    assert!(refs.is_empty());
    assert!(errors.iter().any(|error| {
        error.field == "channels[0].fields.api_base_url"
            && error.message.contains("not available in Quickstart")
    }));
    assert_eq!(
        serde_json::to_value(&cfg).expect("serialize config"),
        before
    );
}

#[test]
fn channel_fields_materialize_credential_free_channel() {
    let mut cfg = Config::default();
    let mut submission = fresh_submission("bot");
    submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
        "webhook",
        "local",
        &[],
    ))];
    let mut staged = Vec::new();
    let mut errors = Vec::new();

    let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);

    assert!(errors.is_empty(), "apply_into errors: {errors:?}");
    assert!(applied.is_some());
    assert!(channel_exists(&cfg, "webhook", "local"));
}

#[tokio::test]
async fn retired_channel_types_are_rejected_by_quickstart_submit() {
    for submitted_type in ["whatsapp", "whatsapp-web", "whatsapp_web", "slack"] {
        let mut cfg = Config::default();
        let before = serde_json::to_value(&cfg).unwrap();
        let before_dirty_paths = cfg.dirty_paths.clone();
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
            submitted_type,
            "personal",
            &[],
        ))];
        let mut errors = Vec::new();
        let refs = apply_channels(&mut cfg, &submission.channels, &mut errors, None);

        let rejected = errors
            .iter()
            .any(|error| error.field == "channels[0].channel_type");
        assert!(
            rejected,
            "retired channel type `{submitted_type}` must be rejected at submit: {errors:?}"
        );
        assert!(refs.is_empty());
        assert_eq!(serde_json::to_value(&cfg).unwrap(), before);
        assert_eq!(cfg.dirty_paths, before_dirty_paths);
        assert!(
            !toml::to_string(&cfg).unwrap().contains("[channels."),
            "a rejected retired channel must not persist any section: {}",
            toml::to_string(&cfg).unwrap()
        );
    }
}

#[tokio::test]
async fn peer_groups_persist_to_canonical_section() {
    let mut submission = fresh_submission("bot");
    submission.channels = vec![SelectorChoice::Fresh(fresh_channel(
        "telegram",
        "tg",
        &[("bot_token", "tok-a")],
    ))];
    submission.peer_groups = vec![zeroclaw_config::presets::QuickstartPeerGroup {
        name: "team".into(),
        channel: "telegram.tg".into(),
        external_peers: vec!["*".into()],
        ignore: vec![],
    }];

    let (dir, _applied) = apply_to_temp(submission).await;
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        raw.contains("[peer_groups.team]"),
        "Quickstart must serialize peer groups through canonical snake_case paths:\n{raw}"
    );
    assert!(
        !raw.contains("[peer-groups.team]"),
        "Quickstart must not write the stale kebab-case peer-groups path:\n{raw}"
    );

    let reloaded = reload(&dir);
    let group = reloaded
        .peer_groups
        .get("team")
        .expect("peer group persisted");
    assert_eq!(group.channel, "telegram.tg");
    assert_eq!(group.external_peers, vec!["*".to_string()]);
}

#[tokio::test]
async fn model_catalog_with_config_uses_native_endpoint_when_credentialed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Native /models endpoint advertising a model that is NOT in the
    // static models.dev snapshot (a freshly released Grok).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "grok-4.5-native-only"},
                {"id": "grok-4.3"}
            ]
        })))
        .mount(&server)
        .await;

    let mut config = Config::default();
    config.providers.models.xai.insert(
        "default".to_string(),
        zeroclaw_config::schema::XaiModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                api_key: Some("xai-test-key".to_string()),
                uri: Some(server.uri()),
                ..Default::default()
            },
        },
    );

    let (models, _pricing, live) = model_catalog_with_config(Some(&config), "xai.default").await;

    assert!(live, "credentialed native listing must report live=true");
    assert!(
        models.iter().any(|m| m == "grok-4.5-native-only"),
        "native /models result must surface the freshly-released model \
         that models.dev does not carry; got {models:?}"
    );
}

#[tokio::test]
async fn model_catalog_with_config_resolves_named_alias_endpoint() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "grok-named-alias-native"}]
        })))
        .mount(&server)
        .await;

    let mut config = Config::default();
    // Non-default alias name to prove the dotted ref targets it precisely.
    config.providers.models.xai.insert(
        "prod".to_string(),
        zeroclaw_config::schema::XaiModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                api_key: Some("xai-test-key".to_string()),
                uri: Some(server.uri()),
                ..Default::default()
            },
        },
    );

    let (models, _pricing, live) = model_catalog_with_config(Some(&config), "xai.prod").await;

    assert!(live);
    assert!(
        models.iter().any(|m| m == "grok-named-alias-native"),
        "dotted `<family>.<alias>` selector must resolve that alias's \
         configured endpoint; got {models:?}"
    );
}
