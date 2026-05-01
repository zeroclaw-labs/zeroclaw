//! Config Schema Migration Tests
//!
//! Validates V1→V2 migration via V1Compat, including the full validation pipeline.

use zeroclaw::config::migration::{self, CURRENT_SCHEMA_VERSION, V1Compat};

fn migrate(toml_str: &str) -> zeroclaw::config::Config {
    let mut table: toml::Table = toml::from_str(toml_str).expect("failed to parse table");
    migration::prepare_table(&mut table);
    let prepared = toml::to_string(&table).expect("failed to re-serialize");
    let compat: V1Compat = toml::from_str(&prepared).expect("failed to deserialize");
    compat.into_config()
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

    let entry = &config.providers.models["openrouter"];
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

    let entry = &config.providers.models["openrouter"];
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
            .fallback_provider()
            .and_then(|e| e.api_key.as_deref()),
        Some("sk-ant")
    );
    assert_eq!(config.providers.fallback.as_deref(), Some("anthropic"));
    assert_eq!(
        config
            .providers
            .fallback_provider()
            .and_then(|e| e.model.as_deref()),
        Some("claude-opus")
    );
    assert!(
        (config
            .providers
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
    assert_eq!(config.providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        config.providers.models["openrouter"].api_key.as_deref(),
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

    assert_eq!(config.providers.fallback.as_deref(), Some("default"));
    assert_eq!(
        config.providers.models["default"].api_key.as_deref(),
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

    assert_eq!(config.providers.fallback.as_deref(), Some("ollama"));
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

    assert!(
        migrated.contains("# Agent tuning"),
        "section comment preserved"
    );
    assert!(
        migrated.contains("# keep it tight"),
        "inline comment preserved"
    );
    // Channel aliasing restructures [channels_config.matrix] into
    // [channels_config.matrix.default], so inline comments on matrix keys
    // do not survive the key move — that is expected.
    assert!(migrated.contains("[providers"), "providers section added");
    assert!(!migrated.contains("room_id"), "room_id removed");
}

#[test]
fn migrate_file_returns_none_when_current() {
    let raw = r#"
schema_version = 3

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

    let config = migrate(&migrated_toml);
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(config.providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        config.providers.models["openrouter"].api_key.as_deref(),
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
    expected.providers.fallback = Some("walk-provider".into());
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
    expected
        .providers
        .models
        .insert("walk-provider".into(), entry);
    expected.providers.models.insert(
        "other-profile".into(),
        ModelProviderConfig {
            base_url: Some("https://other.example.com".into()),
            name: Some("other".into()),
            ..Default::default()
        },
    );
    // Provider fields are now resolved directly — no cache needed.

    // Compare providers.
    assert_eq!(v0.providers.fallback, expected.providers.fallback);
    assert_eq!(v0.providers.models.len(), expected.providers.models.len());
    for (key, v0_entry) in &v0.providers.models {
        let exp = expected
            .providers
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
    assert_eq!(config.providers.fallback.as_deref(), Some("openrouter"));
    assert_eq!(
        config
            .providers
            .fallback_provider()
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
schema_version = 3

[providers]
fallback = "openrouter"

[providers.models.openrouter]
api_key = "sk-or-..."
pricing = { opus = 15.0, sonnet = 3.0 }
"#,
    );

    let entry = &config.providers.models["openrouter"];
    assert_eq!(entry.pricing.get("opus").copied(), Some(15.0));
    assert_eq!(entry.pricing.get("sonnet").copied(), Some(3.0));
}

#[test]
fn pricing_split_dimensions() {
    let config = migrate(
        r#"
schema_version = 3

[providers]
fallback = "anthropic"

[providers.models.anthropic]
api_key = "sk-ant"
pricing = { "opus.input" = 15.0, "opus.output" = 75.0 }
"#,
    );

    let entry = &config.providers.models["anthropic"];
    assert_eq!(entry.pricing.get("opus.input").copied(), Some(15.0));
    assert_eq!(entry.pricing.get("opus.output").copied(), Some(75.0));
}

#[test]
fn pricing_validation_rejects_negative() {
    let config = migrate(
        r#"
schema_version = 3

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
    // A config already at V3 with the new shapes should round-trip cleanly.
    let config = migrate(
        r#"
schema_version = 3

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
fn memory_legacy_db_url_dropped_without_panic() {
    // db_url moved to [storage]; we drop it silently to avoid unknown-key
    // warnings during migration.
    let config = migrate(
        r#"
[memory]
backend = "sqlite"
auto_save = false
db_url = "postgres://user:pass@localhost/db"
"#,
    );

    assert_eq!(config.memory.backend, "sqlite");
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
    assert!(tg.enabled);
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
    assert!(dc.enabled);
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
        config.providers.models["openrouter"].api_key.as_deref(),
        Some("sk-test")
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
