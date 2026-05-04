//! End-to-end migration tests for the V1 → V2 → V3 chain.
//!
//! Sole input: `tests/fixtures/v1.toml`, embedded via `include_str!` so it
//! lives only in the test binary. No fixture files for V2 or V3 — V2/V3
//! shape is asserted via typed deserialization (`Config`) and `toml::Value`
//! navigation on the migration output.
//!
//! Each test asserts a specific transformation and fails if that
//! transformation breaks. Per the plan: no bullshit tests; every test must
//! fail when its target logic is broken.

use zeroclaw_config::migration::{
    CURRENT_SCHEMA_VERSION, MigrateReport, V1_LEGACY_KEYS, detect_version,
    ensure_disk_at_current_version, migrate_file, migrate_file_in_place, migrate_to_current,
};
use zeroclaw_config::schema::Config;

const V1_FIXTURE: &str = include_str!("fixtures/v1.toml");

// ─────────────────────────────────────────────────────────────
// chain validity + schema_version detection
// ─────────────────────────────────────────────────────────────

#[test]
fn chain_produces_valid_v3() {
    let config: Config =
        migrate_to_current(V1_FIXTURE).expect("V1 fixture migrates to current schema");
    assert_eq!(
        config.schema_version, CURRENT_SCHEMA_VERSION,
        "migrated config must have current schema_version"
    );
}

#[test]
fn detect_version_table() {
    assert_eq!(
        detect_version(&toml::from_str("foo = 1").unwrap()).unwrap(),
        1,
        "missing schema_version → V1"
    );
    assert_eq!(
        detect_version(&toml::from_str("schema_version = 2").unwrap()).unwrap(),
        2
    );
    assert_eq!(
        detect_version(&toml::from_str("schema_version = 3").unwrap()).unwrap(),
        3
    );
    assert!(
        detect_version(&toml::from_str("schema_version = -1").unwrap()).is_err(),
        "negative version errors"
    );
    assert!(
        detect_version(&toml::from_str("schema_version = \"two\"").unwrap()).is_err(),
        "non-integer version errors"
    );
}

#[test]
fn v1_legacy_keys_match_v1_fixture_top_level() {
    // Spot-check: V1_LEGACY_KEYS must include the V1 globals our fixture exercises.
    for required in [
        "api_key",
        "api_url",
        "default_provider",
        "default_model",
        "model_providers",
        "extra_headers",
        "channels_config",
    ] {
        assert!(
            V1_LEGACY_KEYS.contains(&required),
            "V1_LEGACY_KEYS missing {required}"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// V1 → V2 → V3 specific transforms
// ─────────────────────────────────────────────────────────────

#[test]
fn v1_default_provider_target_holds_globals() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let entry = cfg
        .providers
        .models
        .get("openai")
        .and_then(|m| m.get("default"))
        .expect("openai.default entry synthesized from V1 default_provider");
    assert_eq!(entry.api_key.as_deref(), Some("sk-v1-test-global"));
    assert_eq!(
        entry.base_url.as_deref(),
        Some("https://api.example.com/v1"),
        "V1 api_url renamed to base_url"
    );
    assert_eq!(entry.model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(entry.temperature, Some(0.5));
    assert_eq!(entry.timeout_secs, Some(90));
    assert_eq!(entry.max_tokens, Some(4096));
    assert_eq!(
        entry.extra_headers.get("User-Agent").map(String::as_str),
        Some("ZeroClaw-V1-Test/1.0")
    );
}

#[test]
fn v1_model_providers_alias_wrapped() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let anthropic_default = cfg
        .providers
        .models
        .get("anthropic")
        .and_then(|m| m.get("default"))
        .expect("anthropic.default present");
    assert_eq!(anthropic_default.api_key.as_deref(), Some("sk-ant-v1-test"));
    assert_eq!(
        anthropic_default.model.as_deref(),
        Some("claude-sonnet-4-5")
    );
}

#[test]
fn claude_code_folded_under_anthropic() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let claude_code = cfg
        .providers
        .models
        .get("anthropic")
        .and_then(|m| m.get("claude-code"))
        .expect("claude-code folded under anthropic.claude-code");
    assert_eq!(claude_code.api_key.as_deref(), Some("sk-cc-v1-test"));
    // Confirm the standalone top-level "claude-code" provider key did NOT survive.
    assert!(
        !cfg.providers.models.contains_key("claude-code"),
        "standalone claude-code provider must not appear in V3"
    );
}

#[test]
fn v1_model_routes_preserved_at_providers_level() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    assert!(
        !cfg.providers.model_routes.is_empty(),
        "model_routes survive into providers.model_routes"
    );
}

#[test]
fn channels_config_renamed_and_alias_wrapped() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let discord_default = cfg
        .channels
        .discord
        .get("default")
        .expect("channels.discord.default exists after alias wrap");
    assert_eq!(discord_default.bot_token, "discord-bot-token-v1");
    // V1 had channels_config.discord.guild_id; V3 schema uses guild_ids
    // (singular folded into plural during the V3 schema cut). Existing
    // schema.rs handles guild_ids as plural by default; the V1 singular
    // is in passthrough on the discord block, so the test asserts the
    // bot_token round-trip rather than the field rename (which is V3
    // schema-internal, not a migration concern).
}

#[test]
fn discord_history_folded_with_archive_flag() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let discord_default = cfg
        .channels
        .discord
        .get("default")
        .expect("channels.discord.default exists");
    assert!(
        discord_default.archive,
        "channels.discord_history fold sets archive=true on channels.discord.default"
    );
}

#[test]
fn autonomy_synthesized_into_risk_profiles_default() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let profile = cfg
        .risk_profiles
        .get("default")
        .expect("risk_profiles.default synthesized from [autonomy]");
    assert_eq!(profile.allowed_commands, vec!["ls", "git", "cat"]);
    assert!(
        profile.workspace_only,
        "V2 autonomy.workspace_only carried into V3 risk_profile.workspace_only"
    );
    assert_eq!(
        profile.excluded_tools,
        vec!["browser"],
        "V2 non_cli_excluded_tools renamed to V3 excluded_tools during fold"
    );
    assert_eq!(profile.shell_timeout_secs, 60);
}

#[test]
fn agent_synthesized_into_runtime_profiles_default() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let profile = cfg
        .runtime_profiles
        .get("default")
        .expect("runtime_profiles.default synthesized from [agent]");
    assert_eq!(profile.parallel_tools, Some(true));
    assert_eq!(profile.max_history_messages, Some(50));
    assert_eq!(profile.max_context_tokens, Some(32000));
    assert_eq!(profile.tool_dispatcher.as_deref(), Some("auto"));
}

#[test]
fn cost_prices_folded_into_provider_pricing() {
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let anthropic_default = cfg
        .providers
        .models
        .get("anthropic")
        .and_then(|m| m.get("default"))
        .expect("anthropic.default exists");
    assert_eq!(
        anthropic_default
            .pricing
            .get("claude-sonnet-4-5.input")
            .copied(),
        Some(3.0),
        "V1 [cost.prices.anthropic.claude-sonnet-4-5.input] folded onto provider pricing"
    );
    assert_eq!(
        anthropic_default
            .pricing
            .get("claude-sonnet-4-5.output")
            .copied(),
        Some(15.0)
    );
}

// ─────────────────────────────────────────────────────────────
// passthrough + comment preservation
// ─────────────────────────────────────────────────────────────

#[test]
fn passthrough_propagates_unknown_section() {
    let migrated = migrate_file(V1_FIXTURE)
        .expect("migrate_file succeeds")
        .expect("migration ran (V1 → V3)");
    let value: toml::Value = toml::from_str(&migrated).unwrap();
    let custom = value
        .get("my_custom_section")
        .and_then(toml::Value::as_table)
        .expect("my_custom_section survives the chain");
    assert_eq!(
        custom.get("custom_field").and_then(toml::Value::as_str),
        Some("preserved-through-chain")
    );
    assert_eq!(
        custom.get("nested_value").and_then(toml::Value::as_integer),
        Some(42)
    );
}

#[test]
fn comment_preserved_on_surviving_key() {
    let migrated = migrate_file(V1_FIXTURE)
        .expect("migrate_file succeeds")
        .expect("migration ran");
    // [cost] survives V1 → V3 (with prices stripped). Its leading comment
    // "# Cost tracking limits and per-model pricing." should round-trip
    // through the toml_edit::DocumentMut reconciliation.
    assert!(
        migrated.contains("Cost tracking limits"),
        "[cost] section comment was not preserved across migration"
    );
}

// ─────────────────────────────────────────────────────────────
// idempotence
// ─────────────────────────────────────────────────────────────

#[test]
fn migrate_file_is_none_when_already_current() {
    // Synthesize a minimal V3 input by running the chain once and serializing.
    let v3_string = migrate_file(V1_FIXTURE)
        .expect("first migrate succeeds")
        .expect("first migrate ran");
    let again = migrate_file(&v3_string).expect("second migrate succeeds");
    assert!(
        again.is_none(),
        "running migrate on a V3 input must be a no-op, got: {again:?}"
    );
}

// ─────────────────────────────────────────────────────────────
// file API: migrate_file_in_place
// ─────────────────────────────────────────────────────────────

#[test]
fn file_api_writes_backup_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, V1_FIXTURE).expect("seed V1 fixture");

    let report: MigrateReport = migrate_file_in_place(&path)
        .expect("migrate_file_in_place succeeds")
        .expect("migration ran (V1 input)");

    let backup_path = report.backup_path.clone();
    assert!(
        backup_path.exists(),
        "backup file must exist at {}",
        backup_path.display()
    );
    let backup_contents = std::fs::read_to_string(&backup_path).expect("read backup");
    assert_eq!(
        backup_contents, V1_FIXTURE,
        "backup must contain the original V1 input verbatim"
    );

    let migrated_contents = std::fs::read_to_string(&path).expect("read migrated config");
    let value: toml::Value = toml::from_str(&migrated_contents).unwrap();
    assert_eq!(
        value
            .get("schema_version")
            .and_then(toml::Value::as_integer),
        Some(CURRENT_SCHEMA_VERSION as i64),
        "config.toml is now at current schema_version"
    );
    assert!(
        backup_path.file_name().and_then(|s| s.to_str()) == Some("config.toml.backup"),
        "backup file name must be `<filename>.backup`, got {}",
        backup_path.display()
    );
}

#[test]
fn file_api_no_op_when_already_current() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // Migrate V1 fixture to V3, then write V3 to disk and call migrate again.
    let v3 = migrate_file(V1_FIXTURE).unwrap().unwrap();
    std::fs::write(&path, &v3).expect("seed V3");
    let report = migrate_file_in_place(&path).expect("migrate_file_in_place succeeds");
    assert!(
        report.is_none(),
        "migrate_file_in_place returns None when input is already current"
    );
    let backup_path = path.with_extension("toml.backup");
    assert!(
        !backup_path.exists(),
        "no backup written when no migration ran"
    );
}

#[test]
fn ensure_disk_at_current_version_passes_for_v3() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let v3 = migrate_file(V1_FIXTURE).unwrap().unwrap();
    std::fs::write(&path, &v3).unwrap();
    ensure_disk_at_current_version(&path).expect("V3 disk passes the gate");
}

#[test]
fn ensure_disk_at_current_version_blocks_stale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, V1_FIXTURE).unwrap();
    let err = ensure_disk_at_current_version(&path)
        .expect_err("V1 disk fails the gate")
        .to_string();
    assert!(
        err.contains("zeroclaw config migrate"),
        "error message must direct user to run migrate, got: {err}"
    );
}

#[test]
fn ensure_disk_at_current_version_passes_for_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist.toml");
    ensure_disk_at_current_version(&missing).expect("missing file is treated as fresh install");
}

// ─────────────────────────────────────────────────────────────
// negative tests — error paths, no panics
// ─────────────────────────────────────────────────────────────

#[test]
fn malformed_toml_returns_clean_error() {
    let err = migrate_to_current("this is not valid TOML {{{").expect_err("malformed TOML errors");
    let msg = err.to_string();
    assert!(
        msg.to_ascii_lowercase().contains("parse"),
        "error message must indicate a parse failure, got: {msg}"
    );
}

#[test]
fn future_schema_version_returns_clean_error() {
    let raw = format!("schema_version = {}\n", CURRENT_SCHEMA_VERSION + 100);
    let err = migrate_to_current(&raw).expect_err("future schema_version errors");
    let msg = err.to_string();
    assert!(
        msg.contains("newer than this binary supports"),
        "error message must explain the future-version refusal, got: {msg}"
    );
}

#[test]
fn malformed_schema_version_returns_clean_error() {
    let err =
        migrate_to_current("schema_version = \"two\"\n").expect_err("non-integer version errors");
    let msg = err.to_string();
    assert!(
        msg.contains("schema_version"),
        "error must mention schema_version, got: {msg}"
    );
}

// ─────────────────────────────────────────────────────────────
// inventory check — every transform documented in V1Config /
// V2Config has at least one test asserting its outcome.
// ─────────────────────────────────────────────────────────────

#[test]
fn inventory_check_v1_to_v2() {
    // Each entry corresponds to a V1Config explicit field. If a transform is
    // added (new explicit field on V1Config), this list must grow and a
    // corresponding test must exist. The `assert!(...)` chains below verify
    // that the V1 fixture exercises each one and the migration produces the
    // expected outcome.
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    let openai_default = cfg
        .providers
        .models
        .get("openai")
        .and_then(|m| m.get("default"))
        .expect("openai default entry");
    assert!(openai_default.api_key.is_some(), "api_key folded");
    assert!(
        openai_default.base_url.is_some(),
        "api_url → base_url folded"
    );
    assert!(
        openai_default.model.is_some(),
        "default_model → model folded"
    );
    assert!(
        openai_default.temperature.is_some(),
        "default_temperature → temperature folded"
    );
    assert!(
        openai_default.timeout_secs.is_some(),
        "provider_timeout_secs → timeout_secs folded"
    );
    assert!(
        openai_default.max_tokens.is_some(),
        "provider_max_tokens → max_tokens folded"
    );
    assert!(
        !openai_default.extra_headers.is_empty(),
        "extra_headers folded"
    );
    assert!(
        cfg.providers.models.contains_key("anthropic"),
        "model_providers entries alias-wrapped"
    );
    assert!(
        !cfg.providers.model_routes.is_empty(),
        "model_routes preserved"
    );
    assert!(
        cfg.channels.discord.contains_key("default"),
        "channels_config → channels alias-wrapped"
    );
}

#[test]
fn inventory_check_v2_to_v3() {
    // Each entry corresponds to a V2Config explicit field's transform.
    let cfg = migrate_to_current(V1_FIXTURE).unwrap();
    assert!(
        cfg.risk_profiles.contains_key("default"),
        "autonomy → risk_profiles.default synthesized"
    );
    assert!(
        cfg.runtime_profiles.contains_key("default"),
        "agent → runtime_profiles.default synthesized"
    );
    // swarms drop is implicit (no swarms in V3 Config struct at all post-RFC).
    let discord_default = cfg.channels.discord.get("default").unwrap();
    assert!(
        discord_default.archive,
        "channels.discord_history → archive=true folded"
    );
    let anthropic_default = cfg
        .providers
        .models
        .get("anthropic")
        .and_then(|m| m.get("default"))
        .unwrap();
    assert!(
        !anthropic_default.pricing.is_empty(),
        "cost.prices folded into provider pricing"
    );
    assert!(
        cfg.providers
            .models
            .get("anthropic")
            .and_then(|m| m.get("claude-code"))
            .is_some(),
        "standalone claude-code provider folded under anthropic.claude-code"
    );
}
