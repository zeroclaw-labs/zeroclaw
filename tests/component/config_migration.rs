//! Config Schema Migration Tests
//!
//! Validates V1→V2 migration via V1Compat, including the full validation pipeline.

use zeroclaw::config::migration::{self, CURRENT_SCHEMA_VERSION};

fn migrate(toml_str: &str) -> zeroclaw::config::Config {
    migration::migrate_to_current(toml_str).expect("migration succeeds")
}

// ─────────────────────────────────────────────────────────────────────────────
// Merge precedence
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn top_level_fields_merge_with_existing_model_providers_entry() {
    let config = migrate(
        r#"
api_key = "sk-test"
default_provider = "openrouter"

[model_providers.openrouter]
base_url = "https://openrouter.ai/api"
"#,
    );

    let entry = &config.providers.models["openrouter"]["default"];
    assert_eq!(entry.api_key.as_deref(), Some("sk-test"));
    assert_eq!(entry.base_url.as_deref(), Some("https://openrouter.ai/api"));
}

#[test]
fn profile_values_take_precedence_over_top_level() {
    let config = migrate(
        r#"
api_key = "sk-top-level"
default_provider = "openrouter"

[model_providers.openrouter]
api_key = "sk-from-profile"
"#,
    );

    let entry = &config.providers.models["openrouter"]["default"];
    assert_eq!(entry.api_key.as_deref(), Some("sk-from-profile"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Edge cases
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn resolved_cache_populated_for_v2_config() {
    let config = migrate(
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
        config
            .providers
            .first_provider()
            .and_then(|e| e.api_key.as_deref()),
        Some("sk-ant")
    );
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("anthropic.default")
    );
    assert_eq!(
        config
            .providers
            .first_provider()
            .and_then(|e| e.model.as_deref()),
        Some("claude-opus")
    );
    assert!(
        (config
            .providers
            .first_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.3)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn room_id_deduped_with_existing_allowed_rooms() {
    let config = migrate(
        r#"
[channels_config.matrix]
homeserver = "https://matrix.org"
access_token = "tok"
room_id = "!abc:matrix.org"
allowed_users = ["@user:matrix.org"]
allowed_rooms = ["!abc:matrix.org", "!other:matrix.org"]
"#,
    );

    let matrix = config.channels.matrix.get("default").unwrap();
    assert_eq!(matrix.allowed_rooms.len(), 2);
}

#[test]
fn already_v2_config_unchanged() {
    let config = migrate(
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
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("openrouter.default")
    );
    assert_eq!(
        config.providers.models["openrouter"]["default"]
            .api_key
            .as_deref(),
        Some("sk-test")
    );
}

#[test]
fn no_default_provider_uses_fallback_name_default() {
    let config = migrate(
        r#"
api_key = "sk-orphan"
"#,
    );

    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("default.default")
    );
    assert_eq!(
        config.providers.models["default"]["default"]
            .api_key
            .as_deref(),
        Some("sk-orphan")
    );
}

#[test]
fn empty_config_produces_valid_v2() {
    let config = migrate("");
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn model_provider_alias_works() {
    let config = migrate(
        r#"
model_provider = "ollama"
"#,
    );

    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("ollama.default")
    );
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

# Agent tuning
[agent]
max_tool_iterations = 5  # keep it tight

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

    // [agent] is removed after synthesis into [runtime_profiles.default], so its
    // section comment (# Agent tuning) and inline key comments (# keep it tight)
    // do not survive. Same for channel aliasing restructuring matrix keys.
    assert!(migrated.contains("[providers"), "providers section added");
    assert!(
        migrated.contains("runtime_profiles"),
        "runtime_profiles synthesised"
    );
    assert!(!migrated.contains("room_id"), "room_id removed");
}

#[test]
fn migrate_file_returns_none_when_current() {
    let raw = r#"
schema_version = 3

[providers]
fallback = ["openrouter.default"]

[providers.models.openrouter.default]
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

    let config = migrate(&migrated_toml);
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    // openrouter (the V1 default_provider) gets the synthesized default
    // alias entry with V1 top-level fields; ollama survives from
    // [model_providers.ollama]. Both must exist; no first-provider
    // assertion (HashMap order is unspecified).
    assert_eq!(
        config.providers.models["openrouter"]["default"]
            .api_key
            .as_deref(),
        Some("rt-key")
    );
    assert!(config.providers.models.contains_key("ollama"));

    let matrix = config.channels.matrix.get("default").unwrap();
    // room_id is no longer on MatrixConfig; migration moves it to allowed_rooms.
    assert!(matrix.allowed_rooms.contains(&"!rt:matrix.org".to_string()));

    // Re-migrating should be a no-op.
    assert!(migration::migrate_file(&migrated_toml).unwrap().is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// claude-code provider rename
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn claude_code_provider_renamed_to_anthropic_in_models() {
    let config = migrate(
        r#"
[providers.models.claude-code]
api_key = "sk-ant-oat01-example"
model = "claude-sonnet-4-6"
"#,
    );
    assert!(
        config.providers.models.contains_key("anthropic"),
        "claude-code model entry should be moved under anthropic"
    );
    assert!(
        !config.providers.models.contains_key("claude-code"),
        "claude-code top-level key should not survive migration"
    );
    assert!(
        config.providers.models["anthropic"].contains_key("claude-code"),
        "entry should appear as anthropic.claude-code alias"
    );
    assert_eq!(
        config.providers.models["anthropic"]["claude-code"]
            .api_key
            .as_deref(),
        Some("sk-ant-oat01-example")
    );
}

#[test]
fn claude_code_fallback_renamed_to_anthropic() {
    let config = migrate(
        r#"
[providers]
fallback = "claude-code"

[providers.models.claude-code]
api_key = "sk-ant-oat01-example"
model = "claude-sonnet-4-6"
"#,
    );
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("anthropic.claude-code")
    );
}

#[test]
fn claude_code_v1_default_provider_renamed_to_anthropic() {
    let config = migrate(
        r#"
default_provider = "claude-code"
api_key = "sk-ant-oat01-example"
"#,
    );
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("anthropic.default")
    );
    assert!(config.providers.models.contains_key("anthropic"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Exhaustive walk
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn exhaustive_walk_no_props_lost() {
    use zeroclaw::config::{Config, ModelProviderConfig};

    let v0 = migrate(
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

    let mut expected = Config::default();
    let mut main_entry = ModelProviderConfig {
        api_key: Some("walk-key".into()),
        base_url: Some("https://walk.example.com".into()),
        api_path: Some("/walk/path".into()),
        model: Some("walk-model".into()),
        temperature: Some(1.11),
        timeout_secs: Some(222),
        max_tokens: Some(333),
        ..Default::default()
    };
    main_entry
        .extra_headers
        .insert("X-Walk".into(), "walk-header".into());
    expected
        .providers
        .models
        .entry("walk-provider".into())
        .or_default()
        .insert("default".to_string(), main_entry);
    expected
        .providers
        .models
        .entry("other-profile".into())
        .or_default()
        .insert(
            "default".to_string(),
            ModelProviderConfig {
                base_url: Some("https://other.example.com".into()),
                name: Some("other".into()),
                ..Default::default()
            },
        );
    // Provider fields are now resolved directly — no cache needed.

    // Compare providers.
    assert_eq!(v0.providers.models.len(), expected.providers.models.len());
    for (type_key, v0_alias_map) in &v0.providers.models {
        let exp_alias_map = expected
            .providers
            .models
            .get(type_key)
            .unwrap_or_else(|| panic!("missing provider type: {type_key}"));
        assert_eq!(
            v0_alias_map.len(),
            exp_alias_map.len(),
            "alias count mismatch for {type_key}"
        );
        for (alias_key, v0_entry) in v0_alias_map {
            let dotted = format!("{type_key}.{alias_key}");
            let exp = exp_alias_map
                .get(alias_key)
                .unwrap_or_else(|| panic!("missing provider alias: {dotted}"));
            assert_eq!(v0_entry.api_key, exp.api_key, "{dotted}");
            assert_eq!(v0_entry.base_url, exp.base_url, "{dotted}");
            assert_eq!(v0_entry.api_path, exp.api_path, "{dotted}");
            assert_eq!(v0_entry.model, exp.model, "{dotted}");
            assert_eq!(v0_entry.temperature, exp.temperature, "{dotted}");
            assert_eq!(v0_entry.timeout_secs, exp.timeout_secs, "{dotted}");
            assert_eq!(v0_entry.max_tokens, exp.max_tokens, "{dotted}");
            assert_eq!(v0_entry.extra_headers, exp.extra_headers, "{dotted}");
            assert_eq!(v0_entry.name, exp.name, "{dotted}");
        }
    }

    // Matrix room_id merged into allowed_rooms by prepare_table.
    let v0_mx = v0.channels.matrix.get("default").unwrap();
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
    let config = migrate(raw);

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("openrouter.default")
    );
    assert_eq!(
        config
            .providers
            .first_provider()
            .and_then(|e| e.model.as_deref()),
        Some("anthropic/claude-sonnet-4.6")
    );

    // Empty room_id must not pollute allowed_rooms.
    let matrix = config.channels.matrix.get("default").unwrap();
    // room_id is no longer on MatrixConfig; migration moves it to allowed_rooms.
    assert!(matrix.allowed_rooms.is_empty());

    // Full validation pipeline must pass.
    config
        .validate()
        .expect("realistic V1 config should pass validation after migration");

    // Legacy keys must not trigger unknown-key warnings.
    let known_keys = {
        let mut keys: Vec<String> = toml::to_string(&zeroclaw::config::Config::default())
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
    let re_config = migrate(&migrated);
    re_config
        .validate()
        .expect("migrated file should also pass validation");
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 channel field plurality
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mattermost_channel_id_migrates_to_channel_ids() {
    let config = migrate(
        r#"
[channels.mattermost]
url = "https://mm.example.com"
bot_token = "tok"
channel_id = "abc123"
allowed_users = ["u1"]
"#,
    );

    let mm = config.channels.mattermost.get("default").unwrap();
    assert_eq!(mm.channel_ids, vec!["abc123".to_string()]);
    assert_eq!(mm.bot_token.as_deref(), Some("tok"));
}

#[test]
fn mattermost_channel_id_deduped_when_channel_ids_present() {
    let config = migrate(
        r#"
[channels.mattermost]
url = "https://mm.example.com"
bot_token = "tok"
channel_id = "abc123"
channel_ids = ["abc123", "def456"]
allowed_users = ["u1"]
"#,
    );

    let mm = config.channels.mattermost.get("default").unwrap();
    assert_eq!(
        mm.channel_ids,
        vec!["abc123".to_string(), "def456".to_string()]
    );
}

#[test]
fn mattermost_bot_token_optional_with_login_credentials() {
    let config = migrate(
        r#"
[channels.mattermost]
url = "https://mm.example.com"
login_id = "bot@example.com"
password = "secret"
channel_ids = ["abc"]
allowed_users = ["u1"]
"#,
    );

    let mm = config.channels.mattermost.get("default").unwrap();
    assert!(mm.bot_token.is_none());
    assert_eq!(mm.login_id.as_deref(), Some("bot@example.com"));
    assert_eq!(mm.password.as_deref(), Some("secret"));
}

#[test]
fn discord_guild_id_migrates_to_guild_ids() {
    let config = migrate(
        r#"
[channels.discord]
bot_token = "tok"
guild_id = "g1"
allowed_users = ["u1"]
"#,
    );

    let dc = config.channels.discord.get("default").unwrap();
    assert_eq!(dc.guild_ids, vec!["g1".to_string()]);
}

#[test]
fn discord_guild_id_wildcard_skipped() {
    let config = migrate(
        r#"
[channels.discord]
bot_token = "tok"
guild_id = "*"
allowed_users = ["u1"]
"#,
    );

    let dc = config.channels.discord.get("default").unwrap();
    assert!(dc.guild_ids.is_empty());
}

#[test]
fn discord_history_only_becomes_discord_with_archive() {
    let config = migrate(
        r#"
[channels.discord-history]
bot_token = "histtok"
channel_ids = ["c1", "c2"]
allowed_users = ["u1"]
"#,
    );

    assert!(!config.channels.discord.is_empty());
    let dc = config.channels.discord.get("default").unwrap();
    assert!(dc.archive);
    assert_eq!(dc.bot_token, "histtok");
    assert_eq!(dc.channel_ids, vec!["c1".to_string(), "c2".to_string()]);
}

#[test]
fn discord_history_and_discord_same_token_sets_archive() {
    let config = migrate(
        r#"
[channels.discord]
bot_token = "tok"
guild_id = "g1"

[channels.discord-history]
bot_token = "tok"
channel_ids = ["c1"]
"#,
    );

    let dc = config.channels.discord.get("default").unwrap();
    assert!(dc.archive);
    assert_eq!(dc.channel_ids, vec!["c1".to_string()]);
    assert_eq!(dc.guild_ids, vec!["g1".to_string()]);
}

#[test]
fn discord_history_different_token_discarded_with_warning() {
    let config = migrate(
        r#"
[channels.discord]
bot_token = "tok-a"

[channels.discord-history]
bot_token = "tok-b"
channel_ids = ["c1"]
"#,
    );

    let dc = config.channels.discord.get("default").unwrap();
    // Different bot_token: archive should NOT be set automatically.
    assert!(!dc.archive);
    assert!(dc.channel_ids.is_empty());
}

#[test]
fn signal_group_id_migrates_to_group_ids() {
    let config = migrate(
        r#"
[channels.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "grpX"
allowed_from = ["+1111111111"]
"#,
    );

    let sg = config.channels.signal.get("default").unwrap();
    assert_eq!(sg.group_ids, vec!["grpX".to_string()]);
    assert!(!sg.dm_only);
}

#[test]
fn signal_group_id_dm_sentinel_migrates_to_dm_only() {
    let config = migrate(
        r#"
[channels.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"
allowed_from = ["+1111111111"]
"#,
    );

    let sg = config.channels.signal.get("default").unwrap();
    assert!(sg.group_ids.is_empty());
    assert!(sg.dm_only);
}

#[test]
fn reddit_subreddit_migrates_to_subreddits() {
    let config = migrate(
        r#"
[channels.reddit]
client_id = "cid"
client_secret = "csec"
refresh_token = "rt"
username = "bot"
subreddit = "rust"
"#,
    );

    let rd = config.channels.reddit.get("default").unwrap();
    assert_eq!(rd.subreddits, vec!["rust".to_string()]);
}

#[test]
fn cost_prices_dropped_during_v2_to_v3() {
    let config = migrate(
        r#"
[cost.prices."anthropic/claude-opus-4-20250514"]
input = 15.0
output = 75.0

[cost.prices."openai/gpt-4o-mini"]
input = 0.15
output = 0.6
"#,
    );

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    // The Cost config still loads, just without the dropped prices field.
    assert!(config.cost.enabled);
}

#[test]
fn pricing_lives_on_model_provider_config() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "openrouter"

[providers.models.openrouter]
api_key = "sk-or-..."
pricing = { opus = 15.0, sonnet = 3.0 }
"#,
    );

    let entry = &config.providers.models["openrouter"]["default"];
    assert_eq!(entry.pricing.get("opus").copied(), Some(15.0));
    assert_eq!(entry.pricing.get("sonnet").copied(), Some(3.0));
}

#[test]
fn pricing_split_dimensions() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
pricing = { "opus.input" = 15.0, "opus.output" = 75.0 }
"#,
    );

    let entry = &config.providers.models["anthropic"]["default"];
    assert_eq!(entry.pricing.get("opus.input").copied(), Some(15.0));
    assert_eq!(entry.pricing.get("opus.output").copied(), Some(75.0));
}

#[test]
fn pricing_validation_rejects_negative() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "openrouter"

[providers.models.openrouter]
api_key = "sk"
pricing = { opus = -1.0 }
"#,
    );
    let err = config
        .validate()
        .expect_err("negative pricing must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("pricing.opus"),
        "error should name the field: {msg}"
    );
    assert!(
        msg.contains(">= 0.0"),
        "error should describe the constraint: {msg}"
    );
}

#[test]
fn already_v3_channel_plurality_unchanged() {
    // A V2 config using V3 field names (plurals) round-trips cleanly through migration.
    let config = migrate(
        r#"
schema_version = 2

[channels.mattermost]
url = "https://mm.example.com"
bot_token = "tok"
channel_ids = ["abc"]
allowed_users = ["u1"]

[channels.discord]
bot_token = "tok"
guild_ids = ["g1"]
allowed_users = ["u1"]

[channels.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_ids = ["grpX"]
allowed_from = ["+1111111111"]

[channels.reddit]
client_id = "cid"
client_secret = "csec"
refresh_token = "rt"
username = "bot"
subreddits = ["rust"]
"#,
    );

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        config
            .channels
            .mattermost
            .get("default")
            .unwrap()
            .channel_ids,
        vec!["abc".to_string()]
    );
    assert_eq!(
        config.channels.discord.get("default").unwrap().guild_ids,
        vec!["g1".to_string()]
    );
    assert_eq!(
        config.channels.signal.get("default").unwrap().group_ids,
        vec!["grpX".to_string()]
    );
    assert_eq!(
        config.channels.reddit.get("default").unwrap().subreddits,
        vec!["rust".to_string()]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Memory migration (#6017)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn memory_sqlite_v2_config_round_trips_without_data_loss() {
    let config = migrate(
        r#"
[memory]
backend = "sqlite"
auto_save = true
sqlite_open_timeout_secs = 30
"#,
    );

    assert_eq!(config.memory.backend, "sqlite");
    assert!(config.memory.auto_save);
    assert_eq!(config.memory.sqlite_open_timeout_secs, Some(30));
    // postgres defaults initialized even for sqlite users
    assert!(!config.memory.postgres.vector_enabled);
    assert_eq!(config.memory.postgres.vector_dimensions, 1536);
}

#[test]
fn memory_sqlite_open_timeout_preserved() {
    let config = migrate(
        r#"
[memory]
backend = "sqlite"
auto_save = true
sqlite_open_timeout_secs = 120
"#,
    );

    assert_eq!(config.memory.sqlite_open_timeout_secs, Some(120));
}

#[test]
fn memory_legacy_pgvector_fields_moved_to_postgres_subsection() {
    let config = migrate(
        r#"
[memory]
backend = "postgres"
auto_save = false
pgvector_enabled = true
pgvector_dimensions = 768
"#,
    );

    assert_eq!(config.memory.backend, "postgres");
    assert!(config.memory.postgres.vector_enabled);
    assert_eq!(config.memory.postgres.vector_dimensions, 768);
}

#[test]
fn memory_legacy_db_url_moved_to_storage_provider_config() {
    let config = migrate(
        r#"
[memory]
backend = "postgres"
auto_save = false
db_url = "postgres://user:pass@localhost/db"
"#,
    );

    assert_eq!(config.memory.backend, "postgres");
    assert_eq!(
        config.storage.provider.config.db_url.as_deref(),
        Some("postgres://user:pass@localhost/db"),
        "db_url must be migrated to [storage.provider.config], not silently dropped"
    );
}

#[test]
fn memory_legacy_db_url_does_not_overwrite_existing_storage_db_url() {
    let config = migrate(
        r#"
[memory]
backend = "postgres"
db_url = "postgres://old@host/old"

[storage.provider.config]
db_url = "postgres://new@host/new"
"#,
    );

    assert_eq!(
        config.storage.provider.config.db_url.as_deref(),
        Some("postgres://new@host/new"),
        "existing [storage.provider.config].db_url must not be overwritten"
    );
}

#[test]
fn memory_postgres_pgvector_and_db_url_both_migrated() {
    let config = migrate(
        r#"
[memory]
backend = "postgres"
pgvector_enabled = true
pgvector_dimensions = 1536
db_url = "postgres://user:pass@host/db"
"#,
    );

    assert!(config.memory.postgres.vector_enabled);
    assert_eq!(config.memory.postgres.vector_dimensions, 1536);
    assert_eq!(
        config.storage.provider.config.db_url.as_deref(),
        Some("postgres://user:pass@host/db")
    );
}

#[test]
fn memory_markdown_backend_round_trips() {
    let config = migrate(
        r#"
[memory]
backend = "markdown"
auto_save = true
"#,
    );

    assert_eq!(config.memory.backend, "markdown");
    assert!(config.memory.auto_save);
}

#[test]
fn memory_none_backend_round_trips() {
    let config = migrate(
        r#"
[memory]
backend = "none"
"#,
    );

    assert_eq!(config.memory.backend, "none");
}

#[test]
fn memory_qdrant_backend_round_trips() {
    let config = migrate(
        r#"
[memory]
backend = "qdrant"

[memory.qdrant]
url = "http://localhost:6334"
collection = "memories"
"#,
    );

    assert_eq!(config.memory.backend, "qdrant");
    assert_eq!(
        config.memory.qdrant.url.as_deref(),
        Some("http://localhost:6334")
    );
    assert_eq!(config.memory.qdrant.collection, "memories");
}

#[test]
fn memory_lucid_backend_round_trips() {
    let config = migrate(
        r#"
[memory]
backend = "lucid"
auto_save = false
"#,
    );

    assert_eq!(config.memory.backend, "lucid");
}

#[test]
fn memory_postgres_subsection_preserved_when_already_present() {
    let config = migrate(
        r#"
[memory]
backend = "postgres"
auto_save = false

[memory.postgres]
vector_enabled = true
vector_dimensions = 1024
"#,
    );

    assert!(config.memory.postgres.vector_enabled);
    assert_eq!(config.memory.postgres.vector_dimensions, 1024);
}

// ─────────────────────────────────────────────────────────────────────────────
// V3: Channel aliasing migration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn v2_flat_telegram_config_wrapped_under_default_alias() {
    let config = migrate(
        r#"
[channels.telegram]
enabled = true
bot_token = "123:ABC"
allowed_users = ["alice"]
"#,
    );

    let tg = config
        .channels
        .telegram
        .get("default")
        .expect("default alias present");
    assert_eq!(tg.bot_token, "123:ABC");
    assert_eq!(tg.allowed_users, vec!["alice"]);
}

#[test]
fn v2_flat_matrix_config_wrapped_under_default_alias() {
    let config = migrate(
        r#"
[channels.matrix]
homeserver = "https://m.org"
access_token = "tok"
allowed_users = ["@u:m.org"]
"#,
    );

    let mx = config
        .channels
        .matrix
        .get("default")
        .expect("default alias present");
    assert_eq!(mx.homeserver, "https://m.org");
    assert_eq!(mx.access_token.as_deref(), Some("tok"));
}

#[test]
fn v3_aliased_channel_not_double_wrapped() {
    // A config already in V3 shape ([channels.telegram.default]) must not be
    // wrapped again under an extra "default" layer.
    let config = migrate(
        r#"
schema_version = 3

[channels.telegram.default]
enabled = true
bot_token = "456:DEF"
allowed_users = []
"#,
    );

    let tg = config
        .channels
        .telegram
        .get("default")
        .expect("default alias present");
    assert_eq!(tg.bot_token, "456:DEF");
    // Must not exist as nested [telegram.default.default]
    assert!(config.channels.telegram.len() == 1);
}

#[test]
fn v2_channels_config_key_wrapped_correctly() {
    // Legacy `[channels_config.discord]` flat config also gets aliasing.
    let config = migrate(
        r#"
[channels_config.discord]
enabled = true
bot_token = "discord-tok"
guild_id = "12345"
"#,
    );

    let dc = config
        .channels
        .discord
        .get("default")
        .expect("default alias present");
    assert_eq!(dc.bot_token, "discord-tok");
}

#[test]
fn v2_swarm_config_dropped_with_no_panic() {
    // V2 [swarms] are dropped silently; the rest of the config migrates cleanly.
    let config = migrate(
        r#"
api_key = "sk-test"
default_provider = "openrouter"

[swarms.my-swarm]
members = ["agent-a", "agent-b"]
"#,
    );

    assert!(config.swarms.is_empty());
    assert_eq!(
        config.providers.models["openrouter"]["default"]
            .api_key
            .as_deref(),
        Some("sk-test")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V3: Provider aliasing migration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn v2_flat_provider_wrapped_under_default_alias() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus-4-5"
"#,
    );

    assert!(
        config.providers.models.contains_key("anthropic"),
        "outer key 'anthropic' must exist"
    );
    assert!(
        config.providers.models["anthropic"].contains_key("default"),
        "inner key 'default' must be created for flat V2 entry"
    );
    assert_eq!(
        config.providers.models["anthropic"]["default"]
            .api_key
            .as_deref(),
        Some("sk-ant")
    );
    assert_eq!(
        config.providers.models["anthropic"]["default"]
            .model
            .as_deref(),
        Some("claude-opus-4-5")
    );
}

// `providers_fallback_updated_to_dotted_alias` deleted: `providers.fallback`
// no longer exists in V3.

#[test]
fn agent_provider_synthesised_into_model_provider_alias() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus-4-5"

[agents.coder]
provider = "anthropic"
"#,
    );

    assert_eq!(
        config.agents["coder"].model_provider.as_str(),
        "anthropic.default",
        "agent with matching provider and no differing brain fields gets model_provider = '<type>.default'"
    );
}

#[test]
fn agent_with_unique_model_gets_per_agent_provider_alias() {
    let config = migrate(
        r#"
schema_version = 2

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus-4-5"

[agents.researcher]
provider = "anthropic"
model = "claude-haiku-4-5"
"#,
    );

    assert_eq!(
        config.agents["researcher"].model_provider.as_str(),
        "anthropic.researcher",
        "agent with a differing model must get its own alias"
    );
    assert!(
        config.providers.models["anthropic"].contains_key("researcher"),
        "per-agent alias entry must be created under providers.models.anthropic"
    );
    assert_eq!(
        config.providers.models["anthropic"]["researcher"]
            .model
            .as_deref(),
        Some("claude-haiku-4-5"),
        "per-agent entry must carry the agent's model override"
    );
}

#[test]
fn already_aliased_providers_not_double_wrapped() {
    let config = migrate(
        r#"
schema_version = 3

[providers]
fallback = ["anthropic.default"]

[providers.models.anthropic.default]
api_key = "sk-ant"
model = "claude-opus-4-5"
"#,
    );

    assert!(
        config.providers.models["anthropic"].contains_key("default"),
        "already-aliased entry must survive migration unchanged"
    );
    assert!(
        !config.providers.models["anthropic"].contains_key("model"),
        "wrapping must not create a spurious 'model' key at the alias level"
    );
    assert_eq!(
        config.providers.models["anthropic"]["default"]
            .api_key
            .as_deref(),
        Some("sk-ant")
    );
    assert_eq!(
        config.providers.first_provider_alias().as_deref(),
        Some("anthropic.default"),
        "already-dotted fallback must not be modified"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V3: Profile synthesis migration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn autonomy_section_synthesises_default_risk_profile() {
    let config = migrate(
        r#"
[autonomy]
level = "full"
max_actions_per_hour = 50
max_cost_per_day_cents = 2000
"#,
    );

    let profile = config
        .risk_profiles
        .get("default")
        .expect("default risk_profile synthesised");
    assert_eq!(
        profile.level,
        zeroclaw_config::autonomy::AutonomyLevel::Full
    );
    assert_eq!(profile.max_actions_per_hour, 50);
    assert_eq!(profile.max_cost_per_day_cents, 2000);
}

#[test]
fn autonomy_non_cli_excluded_tools_not_in_risk_profile() {
    // non_cli_excluded_tools propagates to channel-side excluded_tools, not risk_profiles.
    let config = migrate(
        r#"
[autonomy]
level = "supervised"
non_cli_excluded_tools = ["shell_tool", "file_write"]
"#,
    );

    let profile = config
        .risk_profiles
        .get("default")
        .expect("default risk_profile synthesised");
    assert!(
        profile.excluded_tools.is_empty(),
        "non_cli_excluded_tools must not appear in risk_profiles.default.excluded_tools"
    );
}

#[test]
fn non_cli_excluded_tools_propagated_to_channel_excluded_tools() {
    let config = migrate(
        r#"
[autonomy]
non_cli_excluded_tools = ["shell_tool", "file_write"]

[channels.telegram]
bot_token = "123:ABC"
allowed_users = ["alice"]
"#,
    );

    let tg = config
        .channels
        .telegram
        .get("default")
        .expect("default telegram alias present");
    assert_eq!(
        tg.excluded_tools,
        vec!["shell_tool".to_string(), "file_write".to_string()],
        "non_cli_excluded_tools must propagate to channel excluded_tools"
    );
}

#[test]
fn v2_channel_agent_field_stripped_from_alias() {
    // The V2 `agent` binding on a channel block must not survive into the
    // wrapped [channels.<type>.default] table.
    let config = migrate(
        r#"
[agents.myagent]
provider = "anthropic"
model = "claude-opus"

[channels.telegram]
bot_token = "tok"
agent = "myagent"
allowed_users = ["alice"]
"#,
    );

    // The channel alias must parse cleanly (no unknown field panic).
    let tg = config
        .channels
        .telegram
        .get("default")
        .expect("default telegram alias present");
    assert_eq!(tg.bot_token, "tok");
}

#[test]
fn v2_channel_agent_inverted_onto_agent_channels_list() {
    // V2 channels.<type>.agent = "<alias>" is inverted: agents.<alias>.channels
    // gains "<type>.default" after migration.
    let config = migrate(
        r#"
[agents.worker]
provider = "anthropic"
model = "claude-opus"

[channels.telegram]
bot_token = "tok"
agent = "worker"
allowed_users = ["alice"]
"#,
    );

    let agent = config
        .agents
        .get("worker")
        .expect("agent 'worker' must exist");
    assert!(
        agent.channels.contains(&"telegram.default".to_string()),
        "agent.channels must contain 'telegram.default' after binding inversion"
    );
}

#[test]
fn existing_risk_profiles_default_not_overwritten() {
    // If [risk_profiles.default] is already present, synthesis must not overwrite it.
    let config = migrate(
        r#"
[autonomy]
level = "full"

[risk_profiles.default]
level = "readonly"
"#,
    );

    let profile = config
        .risk_profiles
        .get("default")
        .expect("default risk_profile present");
    assert_eq!(
        profile.level,
        zeroclaw_config::autonomy::AutonomyLevel::ReadOnly,
        "existing risk_profiles.default must not be overwritten by synthesis"
    );
}

#[test]
fn agent_section_synthesises_default_runtime_profile() {
    let config = migrate(
        r#"
[agent]
max_tool_iterations = 25
"#,
    );

    let profile = config
        .runtime_profiles
        .get("default")
        .expect("default runtime_profile synthesised");
    assert_eq!(profile.max_tool_iterations, 25);
}

#[test]
fn no_agent_section_produces_no_runtime_profile() {
    let config = migrate(
        r#"
api_key = "sk-test"
"#,
    );

    // No [agent] section → no synthesised profile
    assert!(config.runtime_profiles.is_empty());
}

#[test]
fn agent_section_removed_after_runtime_profile_synthesis() {
    // V2 [agent] block must not survive in the migrated config.
    let config = migrate(
        r#"
[agent]
max_tool_iterations = 10
parallel_tools = true
"#,
    );

    // Agent fields land in runtime_profiles.default.
    let profile = config
        .runtime_profiles
        .get("default")
        .expect("default runtime_profile synthesised");
    assert_eq!(profile.max_tool_iterations, 10);
    assert_eq!(profile.parallel_tools, Some(true));
}

#[test]
fn risk_profile_merges_security_sandbox_and_resources() {
    let config = migrate(
        r#"
[autonomy]
level = "supervised"

[security.sandbox]
enabled = true
backend = "firejail"
firejail_args = ["--net=none"]

[security.resources]
max_memory_mb = 512
max_cpu_time_seconds = 30
"#,
    );

    let profile = config
        .risk_profiles
        .get("default")
        .expect("default risk_profile synthesised");
    assert_eq!(
        profile.level,
        zeroclaw_config::autonomy::AutonomyLevel::Supervised
    );
    assert_eq!(profile.sandbox_enabled, Some(true));
    assert_eq!(profile.sandbox_backend.as_deref(), Some("firejail"));
    assert_eq!(profile.firejail_args, vec!["--net=none".to_string()]);
    assert_eq!(profile.max_memory_mb, 512);
    assert_eq!(profile.max_cpu_time_seconds, 30);
}

#[test]
fn autonomy_block_removed_after_risk_profile_synthesis() {
    // V2 [autonomy] must not be present in the migrated config.
    // Its fields land in risk_profiles.default instead.
    let config = migrate(
        r#"
[autonomy]
level = "full"
max_actions_per_hour = 100
"#,
    );

    let profile = config
        .risk_profiles
        .get("default")
        .expect("risk profile synthesised");
    assert_eq!(
        profile.level,
        zeroclaw_config::autonomy::AutonomyLevel::Full
    );
    assert_eq!(profile.max_actions_per_hour, 100);
    // [autonomy] is dropped; autonomy field on Config should be at its default.
    assert_eq!(
        config.autonomy.level,
        zeroclaw_config::autonomy::AutonomyLevel::Supervised,
        "[autonomy] block must be removed; Config.autonomy should be default"
    );
}

#[test]
fn security_sandbox_and_resources_removed_after_synthesis() {
    let config = migrate(
        r#"
[security.sandbox]
enabled = true

[security.resources]
max_memory_mb = 256
"#,
    );

    // The risk profile carries the merged values.
    let profile = config
        .risk_profiles
        .get("default")
        .expect("risk profile synthesised from security subsections");
    assert_eq!(profile.sandbox_enabled, Some(true));
    assert_eq!(profile.max_memory_mb, 256);
    // sandbox and resources subsections are removed from [security].
    assert!(
        !config.security.sandbox.enabled.unwrap_or(false) || profile.sandbox_enabled.is_some(),
        "sandbox fields must be in the risk profile after synthesis"
    );
}

#[test]
fn per_agent_risk_profile_carved_out_for_max_depth() {
    let config = migrate(
        r#"
[agents.worker]
provider = "anthropic"
model = "claude-opus"
max_depth = 3
timeout_secs = 60
"#,
    );

    let profile = config
        .risk_profiles
        .get("worker")
        .expect("per-agent risk profile carved out");
    assert_eq!(profile.max_delegation_depth, 3);
    assert_eq!(profile.delegation_timeout_secs, Some(60));
}

#[test]
fn per_agent_runtime_profile_carved_out_for_agentic_flag() {
    let config = migrate(
        r#"
[agents.planner]
provider = "openrouter"
model = "gpt-4"
agentic = true
max_iterations = 20
"#,
    );

    let profile = config
        .runtime_profiles
        .get("planner")
        .expect("per-agent runtime profile carved out");
    assert!(profile.agentic);
    assert_eq!(profile.max_tool_iterations, 20);
}

#[test]
fn memory_namespaces_default_synthesised_from_memory_backend() {
    let config = migrate(
        r#"
[memory]
backend = "sqlite"
auto_save = true
"#,
    );

    let ns = config
        .memory_namespaces
        .get("default")
        .expect("default memory_namespace synthesised");
    assert_eq!(ns.backend.as_deref(), Some("sqlite"));
}

#[test]
fn skill_bundles_default_synthesised_with_skills_directory() {
    let config = migrate(r#"api_key = "sk-test""#);

    let bundle = config
        .skill_bundles
        .get("default")
        .expect("default skill_bundle synthesised");
    assert_eq!(bundle.directory.as_deref(), Some("skills"));
}

#[test]
fn mcp_bundles_default_lists_server_aliases() {
    let config = migrate(
        r#"
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
"#,
    );

    let bundle = config
        .mcp_bundles
        .get("default")
        .expect("default mcp_bundle synthesised");
    let mut servers = bundle.servers.clone();
    servers.sort();
    assert_eq!(
        servers,
        vec!["filesystem".to_string(), "github".to_string()]
    );
}

#[test]
fn matrix_password_auth_without_access_token() {
    let config = migrate(
        r#"
[channels_config.matrix]
homeserver = "https://matrix.org"
user_id = "@bot:matrix.org"
password = "s3cr3t"
allowed_users = ["@user:matrix.org"]
"#,
    );

    let matrix = config.channels.matrix.get("default").unwrap();
    assert!(matrix.access_token.is_none());
    assert_eq!(matrix.password.as_deref(), Some("s3cr3t"));
    assert_eq!(matrix.user_id.as_deref(), Some("@bot:matrix.org"));
}

#[test]
fn minimal_single_agent_install_round_trips() {
    let config = migrate(
        r#"
schema_version = 2

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus"

[providers]
fallback = "anthropic"

[autonomy]
level = "supervised"
workspace_only = true

[agent]
max_tool_iterations = 10
parallel_tools = true
"#,
    );

    assert_eq!(config.schema_version, 3);
    let rp = config.risk_profiles.get("default").unwrap();
    assert_eq!(
        rp.level,
        zeroclaw_config::autonomy::AutonomyLevel::Supervised
    );
    assert!(rp.workspace_only);
    let rt = config.runtime_profiles.get("default").unwrap();
    assert_eq!(rt.max_tool_iterations, 10);
    assert_eq!(rt.parallel_tools, Some(true));
}

#[test]
fn multi_agent_install_with_overrides() {
    let config = migrate(
        r#"
schema_version = 2

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus"

[providers]
fallback = "anthropic"

[agent]
max_tool_iterations = 10

[agents.researcher]
provider = "anthropic"
model = "claude-opus"
max_depth = 2
timeout_secs = 90

[agents.coder]
provider = "anthropic"
model = "claude-sonnet"
agentic = true
max_iterations = 50
"#,
    );

    assert!(config.risk_profiles.contains_key("researcher"));
    let risk = config.risk_profiles.get("researcher").unwrap();
    assert_eq!(risk.max_delegation_depth, 2);
    assert_eq!(risk.delegation_timeout_secs, Some(90));

    assert!(config.runtime_profiles.contains_key("coder"));
    let rt = config.runtime_profiles.get("coder").unwrap();
    assert!(rt.agentic);
    assert_eq!(rt.max_tool_iterations, 50);
}

#[test]
fn multi_channel_binding_inversion() {
    let config = migrate(
        r#"
schema_version = 2

[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus"

[providers]
fallback = "anthropic"

[agents.support]
provider = "anthropic"
model = "claude-opus"

[channels_config.telegram]
bot_token = "123:abc"
agent = "support"
allowed_users = ["user1"]
"#,
    );

    let tg = config.channels.telegram.get("default").unwrap();
    assert_eq!(tg.bot_token, "123:abc");

    let agent = config.agents.get("support").unwrap();
    assert!(agent.channels.contains(&"telegram.default".to_string()));
}

#[test]
fn non_cli_excluded_tools_channel_filter_resolution() {
    let config = migrate(
        r#"
[channels_config.telegram]
bot_token = "123:abc"
allowed_users = ["user1"]

[autonomy]
non_cli_excluded_tools = ["shell_tool", "file_write"]
"#,
    );

    let tg = config.channels.telegram.get("default").unwrap();
    assert!(tg.excluded_tools.contains(&"shell_tool".to_string()));
    assert!(tg.excluded_tools.contains(&"file_write".to_string()));
}

#[test]
fn per_agent_skills_directory_and_memory_namespace_bundle_synthesis() {
    let config = migrate(
        r#"
[providers.models.anthropic]
api_key = "sk-ant"
model = "claude-opus"

[providers]
fallback = "anthropic"

[agents.writer]
provider = "anthropic"
model = "claude-opus"
skills_directory = "agents/writer/skills"
memory_namespace = "writer"
"#,
    );

    assert!(config.skill_bundles.contains_key("writer"));
    let bundle = config.skill_bundles.get("writer").unwrap();
    assert_eq!(bundle.directory.as_deref(), Some("agents/writer/skills"));
}

#[test]
fn swarms_v2_dropped() {
    let config = migrate(
        r#"
[swarms.my_swarm]
strategy = "round_robin"
members = ["agent_a", "agent_b"]
"#,
    );

    assert!(config.swarms.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage migration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn storage_provider_config_round_trips() {
    let config = migrate(
        r#"
[storage.provider.config]
db_url = "postgres://user:pass@host/db"
schema = "myschema"
table = "entries"
connect_timeout_secs = 10
"#,
    );

    assert_eq!(
        config.storage.provider.config.db_url.as_deref(),
        Some("postgres://user:pass@host/db")
    );
    assert_eq!(config.storage.provider.config.schema, "myschema");
    assert_eq!(config.storage.provider.config.table, "entries");
    assert_eq!(
        config.storage.provider.config.connect_timeout_secs,
        Some(10)
    );
}

#[test]
fn storage_defaults_when_absent() {
    let config = migrate(r#"schema_version = 2"#);

    assert_eq!(config.storage.provider.config.schema, "public");
    assert_eq!(config.storage.provider.config.table, "memories");
    assert!(config.storage.provider.config.db_url.is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// Tunnel migration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tunnel_defaults_to_none_when_absent() {
    let config = migrate(r#"schema_version = 2"#);

    assert_eq!(config.tunnel.provider, "none");
    assert!(config.tunnel.cloudflare.is_none());
    assert!(config.tunnel.ngrok.is_none());
    assert!(config.tunnel.tailscale.is_none());
}

#[test]
fn tunnel_cloudflare_config_round_trips() {
    let config = migrate(
        r#"
[tunnel]
provider = "cloudflare"

[tunnel.cloudflare]
token = "cf-tunnel-token"
"#,
    );

    assert_eq!(config.tunnel.provider, "cloudflare");
    let cf = config.tunnel.cloudflare.as_ref().unwrap();
    assert_eq!(cf.token, "cf-tunnel-token");
}

#[test]
fn tunnel_ngrok_config_round_trips() {
    let config = migrate(
        r#"
[tunnel]
provider = "ngrok"

[tunnel.ngrok]
auth_token = "ngrok-token"
domain = "my.ngrok.io"
"#,
    );

    assert_eq!(config.tunnel.provider, "ngrok");
    let ng = config.tunnel.ngrok.as_ref().unwrap();
    assert_eq!(ng.auth_token, "ngrok-token");
    assert_eq!(ng.domain.as_deref(), Some("my.ngrok.io"));
}

#[test]
fn tunnel_tailscale_config_round_trips() {
    let config = migrate(
        r#"
[tunnel]
provider = "tailscale"

[tunnel.tailscale]
funnel = true
hostname = "my-host"
"#,
    );

    assert_eq!(config.tunnel.provider, "tailscale");
    let ts = config.tunnel.tailscale.as_ref().unwrap();
    assert!(ts.funnel);
    assert_eq!(ts.hostname.as_deref(), Some("my-host"));
}

#[test]
fn tunnel_pinggy_config_round_trips() {
    let config = migrate(
        r#"
[tunnel]
provider = "pinggy"

[tunnel.pinggy]
token = "pinggy-token"
"#,
    );

    assert_eq!(config.tunnel.provider, "pinggy");
    let pg = config.tunnel.pinggy.as_ref().unwrap();
    assert_eq!(pg.token.as_deref(), Some("pinggy-token"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixture generation (zeroclaw config generate)
// ─────────────────────────────────────────────────────────────────────────────

const FIXTURES_DIR: &str = "crates/zeroclaw-config/src/fixtures";

fn fixture_raw(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES_DIR}/{name}"))
        .unwrap_or_else(|e| panic!("failed to read {FIXTURES_DIR}/{name}: {e}"))
}

fn generate(version: u32) -> String {
    let v1 = fixture_raw("v1.toml");
    let partial_path = format!("{FIXTURES_DIR}/v{version}.partial.toml");
    let partial = std::fs::read_to_string(&partial_path).ok();
    migration::generate_fixture(version, &v1, partial.as_deref())
        .unwrap_or_else(|e| panic!("generate_fixture({version}) failed: {e}"))
}

#[test]
fn fixture_v1_preserves_raw_field_names() {
    let out = generate(1);
    // V1 uses the pre-migration field names — not the V3 aliases.
    assert!(
        out.contains("room_id"),
        "V1 should have room_id, not allowed_rooms"
    );
    assert!(
        out.contains("guild_id"),
        "V1 should have guild_id, not guild_ids"
    );
    assert!(
        out.contains("subreddit"),
        "V1 should have subreddit, not subreddits"
    );
    assert!(
        out.contains("channels_config"),
        "V1 should use channels_config key"
    );
    assert!(
        !out.contains(".default]"),
        "V1 should not have aliased channel sections"
    );
}

#[test]
fn fixture_v1_has_correct_schema_version() {
    let out = generate(1);
    assert!(out.contains("schema_version = 1"));
}

#[test]
fn fixture_v2_has_correct_schema_version() {
    let out = generate(2);
    assert!(out.contains("schema_version = 2"));
}

#[test]
fn fixture_v2_has_flat_provider_models() {
    let out = generate(2);
    // V2 providers.models is flat: [providers.models.myprovider] not [providers.models.myprovider.default]
    assert!(
        out.contains("[providers.models.myprovider]"),
        "V2 should have flat provider model section"
    );
}

// `fixture_v2_provider_fallback_is_string` deleted: `providers.fallback`
// no longer exists in V3 and the V2 downgrade path no longer emits it.

#[test]
fn fixture_v3_has_correct_schema_version() {
    let out = generate(3);
    assert!(out.contains(&format!("schema_version = {CURRENT_SCHEMA_VERSION}")));
}

#[test]
fn fixture_v3_has_aliased_provider_models() {
    let out = generate(3);
    // V3 providers.models is nested: [providers.models.myprovider.default]
    assert!(
        out.contains("[providers.models.myprovider.default]"),
        "V3 should have aliased provider model section"
    );
}

// `fixture_v3_provider_fallback_is_array` deleted: `providers.fallback`
// no longer exists in V3.

#[test]
fn fixture_v3_has_aliased_channels() {
    let out = generate(3);
    assert!(
        out.contains("[channels.discord.default]"),
        "V3 should have aliased channel sections"
    );
    assert!(
        out.contains("[channels.matrix.default]"),
        "V3 should have aliased matrix channel"
    );
}

#[test]
fn fixture_v3_has_agent_per_enabled_channel() {
    let out = generate(3);
    // All 7 channels in v1.toml have enabled = true; migration synthesizes one agent per channel
    for ch_type in &[
        "discord",
        "matrix",
        "mattermost",
        "reddit",
        "signal",
        "slack",
        "telegram",
    ] {
        assert!(
            out.contains(&format!("[agents.{ch_type}-default]")),
            "V3 should have synthesized agent agents.{ch_type}-default"
        );
        assert!(
            out.contains(&format!("channels = [\"{ch_type}.default\"]")),
            "Synthesized agent for {ch_type} should reference {ch_type}.default"
        );
    }
}

#[test]
fn fixture_v3_channel_configs_have_no_enabled_field() {
    let out = generate(3);
    let lines: Vec<&str> = out.lines().collect();
    let mut in_channel_section = false;
    for line in &lines {
        if line.starts_with("[channels.") {
            in_channel_section = true;
        } else if line.starts_with('[') {
            in_channel_section = false;
        }
        if in_channel_section {
            assert!(
                !line.trim_start().starts_with("enabled ="),
                "Channel config should not contain enabled field: {line}"
            );
        }
    }
}

#[test]
fn fixture_v3_has_risk_profiles() {
    let out = generate(3);
    assert!(
        out.contains("[risk_profiles.default]"),
        "V3 should have risk_profiles.default from autonomy migration"
    );
}

#[test]
fn fixture_v3_has_runtime_profiles() {
    let out = generate(3);
    assert!(
        out.contains("[runtime_profiles.default]"),
        "V3 should have runtime_profiles.default from agent migration"
    );
}

#[test]
fn fixture_v3_does_not_have_legacy_autonomy_section() {
    let out = generate(3);
    assert!(
        !out.contains("\n[autonomy]"),
        "V3 should not re-emit the legacy [autonomy] section"
    );
}

#[test]
fn fixture_v3_does_not_have_legacy_agent_section() {
    let out = generate(3);
    assert!(
        !out.contains("\n[agent]"),
        "V3 should not re-emit the legacy [agent] section"
    );
}

#[test]
fn fixture_v2_and_v3_are_comparable_in_size() {
    let v2 = generate(2);
    let v3 = generate(3);
    let v2_lines = v2.lines().count();
    let v3_lines = v3.lines().count();
    // V2 and V3 should be within 20% of each other in line count.
    let ratio = v2_lines as f64 / v3_lines as f64;
    assert!(
        ratio > 0.8 && ratio < 1.2,
        "V2 ({v2_lines} lines) and V3 ({v3_lines} lines) should be comparable in size"
    );
}

#[test]
fn fixture_v3_migrates_to_current_version() {
    let out = generate(3);
    // A V3 fixture loaded through the migration pipeline should already be at
    // current version and require no further migration.
    let config = migrate(&out);
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn fixture_v3_has_gateway_tls() {
    let out = generate(3);
    assert!(
        out.contains("[gateway.tls]"),
        "V3 should have gateway.tls section"
    );
    assert!(
        out.contains("cert_path"),
        "V3 gateway.tls should have cert_path"
    );
}

#[test]
fn fixture_v3_has_tunnel() {
    let out = generate(3);
    assert!(out.contains("[tunnel]"), "V3 should have tunnel section");
    assert!(
        out.contains("[tunnel.tailscale]"),
        "V3 should have tunnel.tailscale section"
    );
}

#[test]
fn fixture_v3_has_model_routes() {
    let out = generate(3);
    assert!(
        out.contains("[[providers.model_routes]]"),
        "V3 should have model_routes entries"
    );
    assert!(
        out.contains("[[providers.embedding_routes]]"),
        "V3 should have embedding_routes entries"
    );
}

#[test]
fn fixture_v3_has_memory_backends() {
    let out = generate(3);
    assert!(
        out.contains("[memory.qdrant]"),
        "V3 should have memory.qdrant section"
    );
    assert!(
        out.contains("[memory.postgres]"),
        "V3 should have memory.postgres section"
    );
}

#[test]
fn fixture_v3_has_matrix_e2ee_fields() {
    let out = generate(3);
    assert!(
        out.contains("access_token"),
        "V3 matrix channel should have access_token"
    );
    assert!(
        out.contains("recovery_key"),
        "V3 matrix channel should have recovery_key"
    );
    assert!(
        out.contains("device_id"),
        "V3 matrix channel should have device_id"
    );
}

#[test]
fn fixture_v3_has_mattermost_login_id() {
    let out = generate(3);
    assert!(
        out.contains("login_id"),
        "V3 mattermost channel should have login_id"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 — agent migration & validation contract
// (Catches the bugs the earlier round of tests missed: kebab/snake drift,
// non-idempotent transforms, and missing per-agent fold of [agent].)
// ─────────────────────────────────────────────────────────────────────────────

/// `prepare_table` applied twice produces identical TOML — one pass is
/// enough; a second pass should be a no-op. Catches non-idempotent rules
/// that would otherwise corrupt round-tripped configs on every save.
#[test]
fn prepare_table_is_idempotent_on_v1_input() {
    let v1 = fixture_raw("v1.toml");
    let mut once: toml::Table = toml::from_str(&v1).expect("v1 fixture parses");
    migration::prepare_table(&mut once);
    let mut twice = once.clone();
    migration::prepare_table(&mut twice);
    let once_serialised = toml::to_string(&once).expect("re-serialize once");
    let twice_serialised = toml::to_string(&twice).expect("re-serialize twice");
    assert_eq!(
        once_serialised, twice_serialised,
        "prepare_table must be idempotent — second pass changed the table"
    );
}

#[test]
fn prepare_table_is_idempotent_on_v3_output() {
    // Run the V3 fixture (already migrated) through prepare_table —
    // every step should be a no-op since the input is already V3-shaped.
    let v3 = generate(3);
    let mut t: toml::Table = toml::from_str(&v3).expect("v3 fixture parses");
    let before = toml::to_string(&t).expect("serialize before");
    migration::prepare_table(&mut t);
    let after = toml::to_string(&t).expect("serialize after");
    assert_eq!(
        before, after,
        "prepare_table on a V3-shaped input must be a no-op"
    );
}

/// V2 `[agent]` section folds onto `[agents.default]` when no
/// user-supplied `[agents.default]` exists.
#[test]
fn v2_global_agent_section_folds_onto_default_agent() {
    let cfg = migrate(
        r#"
schema_version = 2
api_key = "sk-test"

[agent]
max_tool_iterations = 7
max_history_messages = 33
compact_context = false
tool_dispatcher = "xml"
"#,
    );
    let agent = cfg
        .agents
        .get("default")
        .expect("[agent] folded into agents.default");
    assert_eq!(agent.max_tool_iterations, 7);
    assert_eq!(agent.max_history_messages, 33);
    assert!(!agent.compact_context);
    assert_eq!(agent.tool_dispatcher, "xml");
}

/// User-supplied `[agents.default]` wins; legacy `[agent]` fields fold
/// in only where absent on the user-defined entry.
#[test]
fn v2_agent_fold_does_not_overwrite_user_supplied_default() {
    let cfg = migrate(
        r#"
schema_version = 2
api_key = "sk-test"

[agent]
max_tool_iterations = 7

[agents.default]
max_tool_iterations = 99
model_provider = "openrouter.default"
"#,
    );
    let agent = cfg.agents.get("default").expect("default present");
    // User-supplied wins for the field they specified.
    assert_eq!(agent.max_tool_iterations, 99);
    // model_provider survives unmodified.
    assert_eq!(agent.model_provider, "openrouter.default");
}

/// Idempotency of the fold: a V3 input with no `[agent]` table is a no-op
/// at this step.
#[test]
fn v2_agent_fold_is_noop_when_no_legacy_agent_section() {
    let cfg = migrate(
        r#"
schema_version = 3
api_key = "sk-test"

[agents.researcher]
model_provider = "openrouter.default"
"#,
    );
    assert!(cfg.agents.contains_key("researcher"));
    assert!(!cfg.agents.contains_key("default"));
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 — Cron promotion: [cron] subsystem + [[cron.jobs]] → [scheduler] +
// alias-keyed [cron.<id>] map.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn v2_cron_subsystem_fields_migrate_onto_scheduler() {
    let cfg = migrate(
        r#"
schema_version = 2

[cron]
enabled = false
catch_up_on_startup = false
max_run_history = 200
"#,
    );
    assert!(!cfg.scheduler.enabled);
    assert!(!cfg.scheduler.catch_up_on_startup);
    assert_eq!(cfg.scheduler.max_run_history, 200);
    assert!(cfg.cron.is_empty(), "no jobs declared, cron map is empty");
}

#[test]
fn v2_cron_jobs_array_migrates_to_alias_map() {
    let cfg = migrate(
        r#"
schema_version = 2

[[cron.jobs]]
id = "daily-report"
job_type = "shell"
command = "echo report"
schedule = { kind = "cron", expr = "0 9 * * *" }

[[cron.jobs]]
id = "health-check"
job_type = "agent"
prompt = "Check server health"
schedule = { kind = "every", every_ms = 300000 }
"#,
    );
    assert_eq!(cfg.cron.len(), 2);

    let report = cfg.cron.get("daily-report").expect("daily-report present");
    assert_eq!(report.command.as_deref(), Some("echo report"));
    assert_eq!(report.job_type, "shell");

    let health = cfg.cron.get("health-check").expect("health-check present");
    assert_eq!(health.job_type, "agent");
    assert_eq!(health.prompt.as_deref(), Some("Check server health"));
}

#[test]
fn v2_cron_jobs_without_id_get_synthesized_keys() {
    let cfg = migrate(
        r#"
schema_version = 2

[[cron.jobs]]
job_type = "shell"
command = "echo a"
schedule = { kind = "cron", expr = "0 9 * * *" }

[[cron.jobs]]
job_type = "shell"
command = "echo b"
schedule = { kind = "cron", expr = "0 10 * * *" }
"#,
    );
    assert_eq!(cfg.cron.len(), 2);
    assert!(cfg.cron.contains_key("job-0"));
    assert!(cfg.cron.contains_key("job-1"));
}

#[test]
fn v2_cron_user_supplied_scheduler_values_win() {
    let cfg = migrate(
        r#"
schema_version = 2

[cron]
enabled = false
max_run_history = 100

[scheduler]
enabled = true
"#,
    );
    // [scheduler].enabled was set explicitly → preserved.
    assert!(cfg.scheduler.enabled);
    // [scheduler].max_run_history was unset → folded from [cron].
    assert_eq!(cfg.scheduler.max_run_history, 100);
}

#[test]
fn v3_cron_alias_map_passes_through_unchanged() {
    let cfg = migrate(
        r#"
schema_version = 3

[cron.nightly]
job_type = "shell"
command = "echo go"
schedule = { kind = "cron", expr = "0 2 * * *" }
"#,
    );
    assert_eq!(cfg.cron.len(), 1);
    let job = cfg.cron.get("nightly").expect("nightly present");
    assert_eq!(job.command.as_deref(), Some("echo go"));
}

#[test]
fn v2_cron_promotion_is_idempotent() {
    let raw = r#"
schema_version = 2

[cron]
enabled = false
max_run_history = 33

[[cron.jobs]]
id = "j1"
job_type = "shell"
command = "echo j1"
schedule = { kind = "cron", expr = "0 9 * * *" }
"#;
    // Migrate once: capture the V3 output.
    let once = migrate(raw);
    let serialized = toml::to_string(&once).expect("serialize once");

    // Re-migrate the V3 output as raw input: should be identical.
    let twice = migrate(&serialized);
    let serialized_twice = toml::to_string(&twice).expect("serialize twice");

    assert_eq!(
        serialized, serialized_twice,
        "cron promotion must be idempotent — re-migrating V3 output changed the table"
    );
    assert_eq!(twice.cron.len(), 1);
    assert!(twice.cron.contains_key("j1"));
    assert!(!twice.scheduler.enabled);
    assert_eq!(twice.scheduler.max_run_history, 33);
}

#[test]
fn agent_cron_jobs_field_round_trips() {
    let cfg = migrate(
        r#"
schema_version = 3

[agents.assistant]
model_provider = "openrouter.default"
cron_jobs = ["daily-digest", "health-watch"]

[cron.daily-digest]
job_type = "agent"
prompt = "Summarize yesterday"
schedule = { kind = "cron", expr = "0 9 * * *" }

[cron.health-watch]
job_type = "shell"
command = "echo ok"
schedule = { kind = "every", every_ms = 60000 }
"#,
    );

    let agent = cfg.agents.get("assistant").expect("assistant present");
    assert_eq!(
        agent.cron_jobs,
        vec!["daily-digest".to_string(), "health-watch".to_string()]
    );
    assert_eq!(cfg.cron.len(), 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 — Config::validate() per-agent rules
// ─────────────────────────────────────────────────────────────────────────────

use zeroclaw::config::schema::{
    Config, DelegateAgentConfig, ModelProviderConfig, RiskProfileConfig, SkillBundleConfig,
};

fn cfg_with_provider() -> Config {
    let mut c = Config::default();
    c.providers
        .models
        .entry("openrouter".into())
        .or_default()
        .insert("default".into(), ModelProviderConfig::default());
    c
}

#[test]
fn validate_rejects_agent_with_empty_model_provider() {
    let mut c = cfg_with_provider();
    c.agents.insert("ok".into(), DelegateAgentConfig::default()); // model_provider = ""
    let err = c.validate().unwrap_err().to_string();
    assert!(
        err.contains("agents.ok.model-provider"),
        "expected error path agents.ok.model-provider, got: {err}"
    );
}

#[test]
fn validate_rejects_dangling_model_provider_reference() {
    let mut c = cfg_with_provider();
    c.agents.insert(
        "dangly".into(),
        DelegateAgentConfig {
            model_provider: "anthropic.work".into(),
            ..Default::default()
        },
    );
    let err = c.validate().unwrap_err().to_string();
    assert!(
        err.contains("anthropic.work")
            && err.contains("not configured")
            && err.contains("agents.dangly.model-provider"),
        "expected dangling-ref message keyed on agents.dangly.model-provider, got: {err}"
    );
}

#[test]
fn validate_accepts_agent_with_valid_alias_refs() {
    let mut c = cfg_with_provider();
    c.risk_profiles
        .insert("strict".into(), RiskProfileConfig::default());
    c.skill_bundles
        .insert("rust".into(), SkillBundleConfig::default());
    c.agents.insert(
        "good".into(),
        DelegateAgentConfig {
            model_provider: "openrouter.default".into(),
            risk_profile: "strict".into(),
            skill_bundles: vec!["rust".into()],
            ..Default::default()
        },
    );
    c.validate().expect("valid agent should pass validation");
}

#[test]
fn validate_rejects_dangling_skill_bundle_reference() {
    let mut c = cfg_with_provider();
    c.agents.insert(
        "missing-bundle".into(),
        DelegateAgentConfig {
            model_provider: "openrouter.default".into(),
            skill_bundles: vec!["nonexistent".into()],
            ..Default::default()
        },
    );
    let err = c.validate().unwrap_err().to_string();
    assert!(
        err.contains("agents.missing-bundle.skill_bundles[0]") && err.contains("nonexistent"),
        "expected dangling skill_bundles ref, got: {err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// V3 — kebab-case path round-trip on agents
// (Covers the bug where snake-case format!("agents.{alias}.model_provider")
// returned "Unknown property" because get_prop only knows kebab.)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn agent_field_paths_round_trip_through_kebab() {
    use zeroclaw::config::Config;
    let mut c = Config::default();
    // Bring a default agent into existence so the nested-set routing
    // through HashMap<String, DelegateAgentConfig> has a target.
    c.create_map_key("agents", "researcher")
        .expect("create agent alias");

    // Every alias-ref agent field on its kebab path should accept a
    // value through set_prop and round-trip through get_prop.
    let path_value_pairs: &[(&str, &str)] = &[
        ("agents.researcher.model-provider", "anthropic.default"),
        ("agents.researcher.risk-profile", "strict"),
        ("agents.researcher.runtime-profile", "fast"),
        ("agents.researcher.memory-namespace", "team-a"),
        ("agents.researcher.skill-bundles", "[\"rust\", \"python\"]"),
        ("agents.researcher.knowledge-bundles", "[\"design-docs\"]"),
        ("agents.researcher.mcp-bundles", "[\"filesystem\"]"),
        ("agents.researcher.channels", "[\"telegram.default\"]"),
    ];
    for (path, value) in path_value_pairs {
        c.set_prop(path, value)
            .unwrap_or_else(|e| panic!("set_prop({path}, {value}) failed: {e}"));
        let read = c
            .get_prop(path)
            .unwrap_or_else(|e| panic!("get_prop({path}) failed: {e}"));
        assert!(
            !read.is_empty() && read != "<unset>",
            "round-trip readback empty for {path} (got {read:?})"
        );
    }

    // Negative: snake-case form is rejected so nobody can accidentally
    // bypass the kebab contract.
    assert!(
        c.set_prop("agents.researcher.model_provider", "x").is_err(),
        "snake_case property name must be rejected; only kebab-case is valid"
    );
}
