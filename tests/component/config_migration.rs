//! Config Schema Migration Tests
//!
//! Validates V1→V2 migration via V1Compat, including the full validation pipeline.

use daemonclaw::config::migration::{self, CURRENT_SCHEMA_VERSION, V1Compat};
use daemonclaw::config::providers::ProvidersConfig;

fn migrate(toml_str: &str) -> (daemonclaw::config::Config, ProvidersConfig) {
    let mut table: toml::Table = toml::from_str(toml_str).expect("failed to parse table");
    migration::prepare_table(&mut table);
    let prepared = toml::to_string(&table).expect("failed to re-serialize");
    let compat: V1Compat = toml::from_str(&prepared).expect("failed to deserialize");
    let (config, providers, _proxy) = compat.into_config_with_providers();
    (config, providers)
}

// ─────────────────────────────────────────────────────────────────────────────
// Merge precedence
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn top_level_fields_merge_with_existing_model_providers_entry() {
    let (_config, providers) = migrate(
        r#"
api_key = "sk-test"
default_provider = "openrouter"

[model_providers.openrouter]
base_url = "https://openrouter.ai/api"
"#,
    );

    let entry = &providers.models["openrouter"];
    assert_eq!(entry.api_key.as_deref(), Some("sk-test"));
    assert_eq!(entry.base_url.as_deref(), Some("https://openrouter.ai/api"));
}

#[test]
fn profile_values_take_precedence_over_top_level() {
    let (_config, providers) = migrate(
        r#"
api_key = "sk-top-level"
default_provider = "openrouter"

[model_providers.openrouter]
api_key = "sk-from-profile"
"#,
    );

    let entry = &providers.models["openrouter"];
    assert_eq!(entry.api_key.as_deref(), Some("sk-from-profile"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Edge cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn resolved_cache_populated_for_v2_config() {
    let (_config, providers) = migrate(
        r#"
schema_version = 2

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus"
temperature = 0.3
"#,
    );

    assert_eq!(
        providers
            .fallback_provider()
            .and_then(|e| e.api_key.as_deref()),
        Some("sk-ant")
    );
    assert_eq!(providers.fallback.as_deref(), Some("anthropic"));
    assert_eq!(
        providers
            .fallback_provider()
            .and_then(|e| e.model.as_deref()),
        Some("claude-opus")
    );
    assert!(
        (providers
            .fallback_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.3)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn room_id_deduped_with_existing_allowed_rooms() {
    let (config, _providers) = migrate(
        r#"
[channels_config.matrix]
homeserver = "https://matrix.org"
access_token = "tok"
room_id = "!abc:matrix.org"
allowed_users = ["@user:matrix.org"]
allowed_rooms = ["!abc:matrix.org", "!other:matrix.org"]
"#,
    );

    let matrix = config.channels.matrix.as_ref().unwrap();
    assert_eq!(matrix.allowed_rooms.len(), 2);
}

#[test]
fn already_v2_config_unchanged() {
    let (config, providers) = migrate(
        r#"
schema_version = 2

[providers]
fallback = "openrouter"

[providers.models.openrouter]
api_key = "sk-test"
model = "claude"
"#,
    );

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        providers.models["openrouter"].api_key.as_deref(),
        Some("sk-test")
    );
}

#[test]
fn no_default_provider_uses_fallback_name_default() {
    let (_config, providers) = migrate(
        r#"
api_key = "sk-orphan"
"#,
    );

    assert_eq!(providers.fallback.as_deref(), Some("default"));
    assert_eq!(
        providers.models["default"].api_key.as_deref(),
        Some("sk-orphan")
    );
}

#[test]
fn empty_config_produces_valid_v2() {
    let (config, _providers) = migrate("");
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn model_provider_alias_works() {
    let (_config, providers) = migrate(
        r#"
model_provider = "ollama"
"#,
    );

    assert_eq!(providers.fallback.as_deref(), Some("ollama"));
}

// ─────────────────────────────────────────────────────────────────────────────
// File-level migration (comment preservation)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn migrate_file_preserves_comments() {
    let raw = r#"
# Global settings
schema_version = 0

api_key = "sk-test"          # my API key
default_provider = "openrouter"

# Matrix channel
[channels_config.matrix]
homeserver = "https://matrix.org"  # production server
access_token = "tok"
room_id = "!abc:matrix.org"
allowed_users = ["@user:matrix.org"]
"#;
    let migrated = migration::migrate_file(raw)
        .unwrap()
        .expect("should migrate");

    assert!(
        migrated.contains("# Matrix channel"),
        "section comment preserved"
    );
    assert!(
        migrated.contains("# production server"),
        "inline comment preserved"
    );
    assert!(migrated.contains("[providers"), "providers section added");
    assert!(!migrated.contains("room_id ="), "room_id removed");
}

#[test]
fn migrate_file_returns_none_when_current() {
    let raw = r#"
schema_version = 2

[providers]
fallback = "openrouter"

[providers.models.openrouter]
api_key = "sk-test"
"#;
    assert!(migration::migrate_file(raw).unwrap().is_none());
}

#[test]
fn migrate_file_round_trips() {
    let raw = r#"
api_key = "rt-key"
default_provider = "openrouter"
default_model = "claude"
default_temperature = 0.5
provider_timeout_secs = 60

[model_providers.ollama]
base_url = "http://localhost:11434"

[channels_config.matrix]
homeserver = "https://matrix.org"
access_token = "tok"
room_id = "!rt:matrix.org"
allowed_users = ["@u:m"]
"#;
    let migrated_toml = migration::migrate_file(raw)
        .unwrap()
        .expect("should migrate");

    let (config, providers) = migrate(&migrated_toml);
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        providers.models["openrouter"].api_key.as_deref(),
        Some("rt-key")
    );
    assert!(providers.models.contains_key("ollama"));

    let matrix = config.channels.matrix.as_ref().unwrap();
    // room_id is no longer on MatrixConfig; migration moves it to allowed_rooms.
    assert!(matrix.allowed_rooms.contains(&"!rt:matrix.org".to_string()));

    // Re-migrating should be a no-op.
    assert!(migration::migrate_file(&migrated_toml).unwrap().is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// Exhaustive walk
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn exhaustive_walk_no_props_lost() {
    use daemonclaw::config::{Config, ModelProviderConfig};

    let (v0, v0_providers) = migrate(
        r#"
api_key = "walk-key"
api_url = "https://walk.example.com"
api_path = "/walk/path"
default_provider = "walk-provider"
default_model = "walk-model"
default_temperature = 1.11
provider_timeout_secs = 222
provider_max_tokens = 333

[extra_headers]
X-Walk = "walk-header"

[model_providers.other-profile]
base_url = "https://other.example.com"
name = "other"

[channels_config.matrix]
homeserver = "https://walk-matrix.org"
access_token = "walk-token"
room_id = "!walk:matrix.org"
allowed_users = ["@walk:matrix.org"]
allowed_rooms = ["!existing:matrix.org"]
"#,
    );

    let expected = Config::default();
    let mut expected_providers = ProvidersConfig::default();
    expected_providers.fallback = Some("walk-provider".into());
    let mut entry = ModelProviderConfig {
        api_key: Some("walk-key".into()),
        base_url: Some("https://walk.example.com".into()),
        api_path: Some("/walk/path".into()),
        model: Some("walk-model".into()),
        temperature: Some(1.11),
        timeout_secs: Some(222),
        max_tokens: Some(333),
        ..Default::default()
    };
    entry
        .extra_headers
        .insert("X-Walk".into(), "walk-header".into());
    expected_providers
        .models
        .insert("walk-provider".into(), entry);
    expected_providers.models.insert(
        "other-profile".into(),
        ModelProviderConfig {
            base_url: Some("https://other.example.com".into()),
            name: Some("other".into()),
            ..Default::default()
        },
    );
    // Provider fields are now resolved directly — no cache needed.

    // Compare providers.
    assert_eq!(v0_providers.fallback, expected_providers.fallback);
    assert_eq!(v0_providers.models.len(), expected_providers.models.len());
    for (key, v0_entry) in &v0_providers.models {
        let exp = expected_providers
            .models
            .get(key)
            .unwrap_or_else(|| panic!("missing provider entry: {key}"));
        assert_eq!(v0_entry.api_key, exp.api_key, "{key}");
        assert_eq!(v0_entry.base_url, exp.base_url, "{key}");
        assert_eq!(v0_entry.api_path, exp.api_path, "{key}");
        assert_eq!(v0_entry.model, exp.model, "{key}");
        assert_eq!(v0_entry.temperature, exp.temperature, "{key}");
        assert_eq!(v0_entry.timeout_secs, exp.timeout_secs, "{key}");
        assert_eq!(v0_entry.max_tokens, exp.max_tokens, "{key}");
        assert_eq!(v0_entry.extra_headers, exp.extra_headers, "{key}");
        assert_eq!(v0_entry.name, exp.name, "{key}");
    }

    // Matrix room_id merged into allowed_rooms by prepare_table.
    let v0_mx = v0.channels.matrix.as_ref().unwrap();
    assert!(
        v0_mx
            .allowed_rooms
            .contains(&"!walk:matrix.org".to_string())
    );
    assert!(
        v0_mx
            .allowed_rooms
            .contains(&"!existing:matrix.org".to_string())
    );

    // prop_fields() exhaustive check.
    let v0_props = v0.prop_fields();
    let expected_props = expected.prop_fields();
    for exp in &expected_props {
        if exp.is_secret || exp.display_value == "<unset>" {
            continue;
        }
        let found = v0_props
            .iter()
            .find(|p| p.name == exp.name)
            .unwrap_or_else(|| panic!("prop {} missing after migration", exp.name));
        assert_eq!(found.display_value, exp.display_value, "prop {}", exp.name);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Realistic config: full pipeline (deserialize → migrate → validate)
// ─────────────────────────────────────────────────────────────────────────────

/// Reproduces a real user config: empty sections, known provider name with no
/// api_url, empty room_id, feature-gated channels. Must pass full validation.
#[test]
fn realistic_v1_config_migrates_and_validates() {
    let raw = r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7
provider_timeout_secs = 120
model_routes = []
embedding_routes = []

[model_providers]

[extra_headers]

[observability]
backend = "none"

[autonomy]
level = "supervised"
workspace_only = true

[channels_config]
cli = true

[channels_config.matrix]
enabled = false
homeserver = "https://matrix.org"
access_token = "tok"
room_id = ""
allowed_users = []
allowed_rooms = []

[memory]
backend = "sqlite"
auto_save = true

[gateway]
port = 42617
host = "127.0.0.1"
require_pairing = true
"#;
    let (config, providers) = migrate(raw);

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        providers
            .fallback_provider()
            .and_then(|e| e.model.as_deref()),
        Some("anthropic/claude-sonnet-4.6")
    );

    // Empty room_id must not pollute allowed_rooms.
    let matrix = config.channels.matrix.as_ref().unwrap();
    // room_id is no longer on MatrixConfig; migration moves it to allowed_rooms.
    assert!(matrix.allowed_rooms.is_empty());

    // Full validation pipeline must pass.
    config
        .validate()
        .expect("realistic V1 config should pass validation after migration");

    // Legacy keys must not trigger unknown-key warnings.
    let known_keys = {
        let mut keys: Vec<String> = toml::to_string(&daemonclaw::config::Config::default())
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();
        keys.extend(migration::V1_LEGACY_KEYS.iter().map(|s| s.to_string()));
        keys
    };
    let raw_table: toml::Table = toml::from_str(raw).unwrap();
    let unknown: Vec<&String> = raw_table
        .keys()
        .filter(|k| !known_keys.contains(k))
        .collect();
    assert!(
        unknown.is_empty(),
        "legacy keys flagged as unknown: {unknown:?}"
    );

    // File migration must also work end-to-end.
    let migrated = migration::migrate_file(raw)
        .unwrap()
        .expect("should migrate");
    let (re_config, _re_providers) = migrate(&migrated);
    re_config
        .validate()
        .expect("migrated file should also pass validation");
}
