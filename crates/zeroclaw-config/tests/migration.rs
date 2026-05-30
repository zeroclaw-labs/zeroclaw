//! End-to-end migration tests for the V1 → V2 → V3 chain.
//!
//! Sole input: `fixtures/v1.toml` at the crate root, embedded via
//! `include_str!` so it lives only in the test/cli binary. No fixture
//! files for V2 or V3 — V2/V3 shape is asserted via typed deserialization
//! (`Config`) and `toml::Value` navigation on the migration output.
//!
//! One test per transform listed in the plan's Step 0 ground truth. Each
//! test asserts the destination value present in V3 output; if the migration
//! step that performs the transform is broken, the test fails.

use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::migration::{
    CURRENT_SCHEMA_VERSION, GenerateOptions, MigrateReport, detect_version, encrypt_secret_strings,
    ensure_disk_at_current_version, generate, migrate_file, migrate_file_in_place,
    migrate_to_current,
};
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::v2::V2Config;
use zeroclaw_config::secrets::SecretStore;

const V1_FIXTURE: &str = include_str!("../fixtures/v1.toml");

fn v3_config() -> Config {
    migrate_to_current(V1_FIXTURE).expect("V1 fixture migrates to current schema")
}

fn v3_value() -> toml::Value {
    let migrated = migrate_file(V1_FIXTURE)
        .expect("migrate_file succeeds")
        .expect("migration ran (V1 → V3)");
    toml::from_str(&migrated).expect("migrate_file output parses as TOML")
}

/// Run a V2-shape TOML literal through `V2Config::migrate()` directly. Used by
/// V2→V3-only transform tests where threading data through a V1 fixture would
/// fake a starting state that no real user ever wrote.
///
/// Gate: the V3 output must round-trip as `Config` (no `unknown field`, no
/// type mismatches). This closes the V2-fixture-round-trip gate from the
/// migration plan in one place: every test that calls `migrate_v2` proves
/// its V2 input also produces a V3-loadable config.
fn migrate_v2(input: &str) -> toml::Value {
    let v2: V2Config = toml::from_str(input).expect("V2 input parses as V2Config");
    let value = v2.migrate().expect("V2 → V3 migration succeeds");
    let serialized = toml::to_string(&value).expect("V3 output serializes to TOML");
    let _: Config =
        toml::from_str(&serialized).expect("V3 output parses as Config (schema-round-trip gate)");
    value
}

// ─────────────────────────────────────────────────────────────
// chain validity + schema_version detection
// ─────────────────────────────────────────────────────────────

#[test]
fn chain_produces_valid_v3() {
    let cfg = v3_config();
    assert_eq!(
        cfg.schema_version, CURRENT_SCHEMA_VERSION,
        "migrated config must carry current schema_version"
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

// ─────────────────────────────────────────────────────────────
// V1 globals → V2 [providers] → V3 model_providers.<type>.default
// ─────────────────────────────────────────────────────────────

#[test]
fn v1_default_provider_target_holds_globals() {
    // V1 globals fold into the per-provider entry identified by
    // V1 default_provider. With no matching entry under model_providers,
    // a fresh entry is synthesized and every V1 global lands on it.
    let raw = r#"
api_key = "sk-fold-target"
api_url = "https://api.fold.test"
api_path = "/v1/chat/completions"
default_provider = "openai"
default_model = "gpt-4o-mini"
default_temperature = 0.5
provider_timeout_secs = 90
provider_max_tokens = 4096

[extra_headers]
"User-Agent" = "ZeroClaw-V1-Test/1.0"
"#;
    let cfg = migrate_to_current(raw).expect("V1 globals migrate");
    let entry = cfg
        .providers
        .models
        .find("openai", "default")
        .expect("openai.default synthesized from V1 default_provider");
    assert_eq!(entry.api_key.as_deref(), Some("sk-fold-target"));
    assert_eq!(
        entry.uri.as_deref(),
        Some("https://api.fold.test/v1/chat/completions"),
        "V1 api_url + api_path merged into the per-provider entry's uri"
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
fn v2_model_providers_alias_wrapped() {
    let v3 = migrate_v2(
        r#"
[providers.models.anthropic]
api_key = "sk-ant-v2-test"
model = "claude-sonnet-4-5"
"#,
    );
    let anth = lookup_dotted(&v3, "providers.models.anthropic.default")
        .and_then(toml::Value::as_table)
        .expect("providers.models.anthropic.default present after V2→V3");
    assert_eq!(
        anth.get("api_key").and_then(toml::Value::as_str),
        Some("sk-ant-v2-test")
    );
    assert_eq!(
        anth.get("model").and_then(toml::Value::as_str),
        Some("claude-sonnet-4-5")
    );
}

#[test]
fn claude_code_folded_under_anthropic() {
    let v3 = migrate_v2(
        r#"
[providers.models.claude-code]
api_key = "sk-cc-v2-test"
"#,
    );
    let model_providers = lookup_dotted(&v3, "providers.models")
        .and_then(toml::Value::as_table)
        .expect("providers.models present after V2→V3");
    let cc = model_providers
        .get("anthropic")
        .and_then(toml::Value::as_table)
        .and_then(|a| a.get("claude-code"))
        .and_then(toml::Value::as_table)
        .expect("claude-code folded under providers.models.anthropic.claude-code");
    assert_eq!(
        cc.get("api_key").and_then(toml::Value::as_str),
        Some("sk-cc-v2-test")
    );
    assert!(
        !model_providers.contains_key("claude-code"),
        "standalone claude-code provider must not appear in V3"
    );
}

#[test]
fn v1_model_routes_preserved_at_providers_level() {
    let cfg = v3_config();
    assert!(
        !cfg.model_routes.is_empty(),
        "model_routes survive into model_routes"
    );
}

// ─────────────────────────────────────────────────────────────
// T1, T2 — V1→V2 channel singular→plural folds
// ─────────────────────────────────────────────────────────────

#[test]
fn t1_matrix_room_id_folds_into_allowed_rooms() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.matrix]
enabled = true
homeserver = "https://matrix.org"
access_token = "tok"
room_id = "!fold-test:matrix.org"
allowed_users = ["@u:matrix.org"]
"#;
    let cfg = migrate_to_current(raw).expect("V1 matrix migrates");
    let matrix = cfg
        .channels
        .matrix
        .get("default")
        .expect("channels.matrix.default exists");
    assert!(
        matrix
            .allowed_rooms
            .iter()
            .any(|r| r == "!fold-test:matrix.org"),
        "V1 matrix.room_id was not folded into V3 allowed_rooms[]; got {:?}",
        matrix.allowed_rooms
    );
}

#[test]
fn t2_slack_channel_id_folds_into_channel_ids() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.slack]
enabled = true
bot_token = "xoxb-tok"
channel_id = "C0FOLDTEST"
allowed_users = ["U1"]
"#;
    let cfg = migrate_to_current(raw).expect("V1 slack migrates");
    let slack = cfg
        .channels
        .slack
        .get("default")
        .expect("channels.slack.default exists");
    assert!(
        slack.channel_ids.iter().any(|c| c == "C0FOLDTEST"),
        "V1 slack.channel_id was not folded into V3 channel_ids[]; got {:?}",
        slack.channel_ids
    );
}

// ─────────────────────────────────────────────────────────────
// T3-T6 — V2→V3 channel singular→plural folds
// ─────────────────────────────────────────────────────────────

#[test]
fn t3_discord_guild_id_folds_into_guild_ids() {
    let v3 = migrate_v2(
        r#"
schema_version = 2

[channels.discord]
enabled = true
bot_token = "discord-tok"
guild_id = "FOLDGUILD"
"#,
    );
    let guild_ids = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("discord"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("guild_ids"))
        .and_then(toml::Value::as_array)
        .expect("channels.discord.default.guild_ids array");
    assert!(
        guild_ids.iter().any(|v| v.as_str() == Some("FOLDGUILD")),
        "V2 discord.guild_id was not folded into V3 guild_ids[]; got {:?}",
        guild_ids
    );
}

#[test]
fn t4_mattermost_channel_id_folds_into_channel_ids() {
    let v3 = migrate_v2(
        r#"
schema_version = 2

[channels.mattermost]
enabled = true
url = "https://mm.example.com"
bot_token = "mm-tok"
channel_id = "mm-fold-test"
"#,
    );
    let channel_ids = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("mattermost"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("channel_ids"))
        .and_then(toml::Value::as_array)
        .expect("channels.mattermost.default.channel_ids array");
    assert!(
        channel_ids
            .iter()
            .any(|v| v.as_str() == Some("mm-fold-test")),
        "V2 mattermost.channel_id was not folded into V3 channel_ids[]; got {:?}",
        channel_ids
    );
}

#[test]
fn t5_reddit_subreddit_folds_into_subreddits() {
    let v3 = migrate_v2(
        r#"
schema_version = 2

[channels.reddit]
enabled = true
client_id = "rid"
client_secret = "rsec"
refresh_token = "rrt"
username = "bot"
subreddit = "fold-test"
"#,
    );
    let subreddits = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("reddit"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("subreddits"))
        .and_then(toml::Value::as_array)
        .expect("channels.reddit.default.subreddits array");
    assert!(
        subreddits.iter().any(|v| v.as_str() == Some("fold-test")),
        "V2 reddit.subreddit was not folded into V3 subreddits[]; got {:?}",
        subreddits
    );
}

#[test]
fn t6_signal_group_id_folds_into_group_ids() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.signal]
enabled = true
http_url = "http://127.0.0.1:8686"
account = "+15555550100"
group_id = "fold-test-group"
"#;
    let cfg = migrate_to_current(raw).expect("V1 signal migrates");
    let signal = cfg
        .channels
        .signal
        .get("default")
        .expect("channels.signal.default exists");
    assert!(
        signal.group_ids.iter().any(|g| g == "fold-test-group"),
        "V2 signal.group_id was not folded into V3 group_ids[]; got {:?}",
        signal.group_ids
    );
    assert!(
        !signal.dm_only,
        "non-\"dm\" group_id must not set dm_only=true"
    );
}

// ─────────────────────────────────────────────────────────────
// T7 — channel `enabled` semantics. V3 keeps the V2 boolean on the
// channel config; the runtime gates registration on `cfg.enabled` and
// the migration ports the value through verbatim so an operator's
// "configured but parked" channel survives migration.
// ─────────────────────────────────────────────────────────────

#[test]
fn t7_enabled_false_channel_preserved() {
    let v3 = migrate_v2(
        r#"
schema_version = 2

[channels.webhook]
enabled = false
port = 8080
"#,
    );
    let webhook_enabled = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("webhook"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("enabled"))
        .and_then(toml::Value::as_bool);
    assert_eq!(
        webhook_enabled,
        Some(false),
        "V2 enabled=false must round-trip into V3 channels.webhook.default.enabled; \
         the orchestrator gates registration, not the migration"
    );
}

#[test]
fn t7_enabled_unset_defaults_to_false() {
    let cfg = v3_config();
    let imessage =
        cfg.channels.imessage.get("default").expect(
            "V2 imessage block (peer-auth fields folded into peer_groups) survives into V3",
        );
    assert!(
        !imessage.enabled,
        "V2 missing `enabled` deserializes to V3 enabled = false (matches V2 default)"
    );
}

#[test]
fn t7_enabled_field_preserved_on_surviving_instance() {
    // V3 keeps the `enabled` field on every channel config. Matrix in
    // the V1 fixture has enabled = true; migration ports the value
    // through verbatim and the typed config exposes it.
    let cfg = v3_config();
    let matrix_default = cfg
        .channels
        .matrix
        .get("default")
        .expect("channels.matrix.default in migrated config");
    assert!(
        matrix_default.enabled,
        "V2 enabled = true must port through to V3 channels.matrix.default.enabled"
    );
}

// ─────────────────────────────────────────────────────────────
// discord_history fold (covered already in V2→V3 step) + T7 interaction
// ─────────────────────────────────────────────────────────────

#[test]
fn discord_history_folded_with_archive_flag() {
    let cfg = v3_config();
    let discord = cfg
        .channels
        .discord
        .get("default")
        .expect("channels.discord.default present");
    assert!(
        discord.archive,
        "channels.discord_history fold sets archive=true on channels.discord.default"
    );
}

// ─────────────────────────────────────────────────────────────
// T8 — TTS subsystem promotion
// ─────────────────────────────────────────────────────────────

#[test]
fn t8_tts_subsystem_promoted_to_providers() {
    let value = migrate_v2(
        r#"
schema_version = 2

[tts.openai]
api_key = "sk-tts-openai"
model = "tts-1"
voice = "alloy"

[tts.elevenlabs]
api_key = "el-tts-key"
model_id = "eleven_monolingual_v1"
"#,
    );
    // [tts.openai] should be GONE from [tts] (moved to tts_providers.openai.default)
    let tts_has_openai = value
        .get("tts")
        .and_then(toml::Value::as_table)
        .is_some_and(|t| t.contains_key("openai"));
    assert!(
        !tts_has_openai,
        "V2 [tts.openai] sub-block must be moved out of [tts]"
    );

    // And it should appear at providers.tts.openai.default with the api_key.
    let api_key = lookup_dotted(&value, "providers.tts.openai.default")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("api_key"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        api_key,
        Some("sk-tts-openai"),
        "V2 [tts.openai].api_key did not land at providers.tts.openai.default.api_key"
    );

    // ElevenLabs V2 `model_id` must be renamed to V3 `model`.
    let eleven_default = lookup_dotted(&value, "providers.tts.elevenlabs.default")
        .and_then(toml::Value::as_table)
        .expect("providers.tts.elevenlabs.default present");
    assert_eq!(
        eleven_default.get("model").and_then(toml::Value::as_str),
        Some("eleven_monolingual_v1"),
        "V2 tts.elevenlabs.model_id must be renamed to V3 model on TtsProviderConfig"
    );
    assert!(
        !eleven_default.contains_key("model_id"),
        "V2 model_id must not survive into V3 (it has no slot on TtsProviderConfig)"
    );
}

#[test]
fn t8_tts_default_provider_dropped() {
    let value = migrate_v2(
        r#"
schema_version = 2

[tts]
default_provider = "openai"

[tts.openai]
api_key = "sk-tts"
"#,
    );
    // V3 has no global default-provider concept for TTS; the fold drops it.
    let dp = value
        .get("tts")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default_provider"));
    assert!(
        dp.is_none(),
        "V2 tts.default_provider must be dropped (V3 has no global default-provider for TTS); got {dp:?}"
    );
}

// ─────────────────────────────────────────────────────────────
// T9 + T10 — storage subsystem promotion
// ─────────────────────────────────────────────────────────────

#[test]
fn t9_memory_qdrant_promoted_to_storage() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "qdrant"
auto_save = true

[memory.qdrant]
url = "http://qdrant.example:6333"
collection = "fold_test_memories"
api_key = "qd-key"
"#;
    let cfg = migrate_to_current(raw).expect("V1 memory.qdrant migrates");
    let qdrant = cfg
        .storage
        .qdrant
        .get("default")
        .expect("[memory.qdrant] promoted to [storage.qdrant.default]");
    assert_eq!(qdrant.url.as_deref(), Some("http://qdrant.example:6333"));
    assert_eq!(qdrant.collection, "fold_test_memories");
    assert_eq!(qdrant.api_key.as_deref(), Some("qd-key"));
}

#[test]
fn t9_memory_postgres_vector_fields_promoted() {
    let v3 = migrate_v2(
        r#"
[memory.postgres]
vector_enabled = true
vector_dimensions = 1536
"#,
    );
    let pg = v3
        .get("storage")
        .and_then(toml::Value::as_table)
        .and_then(|s| s.get("postgres"))
        .and_then(toml::Value::as_table)
        .and_then(|p| p.get("default"))
        .and_then(toml::Value::as_table)
        .expect("[memory.postgres] vector fields promoted to [storage.postgres.default]");
    assert_eq!(
        pg.get("vector_enabled").and_then(toml::Value::as_bool),
        Some(true),
        "V2 [memory.postgres] vector_enabled must land at V3 storage.postgres.default.vector_enabled"
    );
    assert_eq!(
        pg.get("vector_dimensions")
            .and_then(toml::Value::as_integer),
        Some(1536)
    );
}

#[test]
fn t9_memory_sqlite_open_timeout_promoted() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "sqlite"
auto_save = true
sqlite_open_timeout_secs = 60
"#;
    let cfg = migrate_to_current(raw).expect("V1 memory.sqlite migrates");
    let sqlite = cfg
        .storage
        .sqlite
        .get("default")
        .expect("storage.sqlite.default exists after sqlite_open_timeout_secs fold");
    assert_eq!(
        sqlite.open_timeout_secs,
        Some(60),
        "V2 memory.sqlite_open_timeout_secs must land at \
         storage.sqlite.default.open_timeout_secs"
    );
}

#[test]
fn t10_storage_provider_postgres_promoted() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[storage.provider.config]
provider = "postgres"
db_url = "postgres://u:p@localhost/zc"
schema = "zc_schema"
table = "memories"
connect_timeout_secs = 42
"#;
    let cfg = migrate_to_current(raw).expect("V1 storage.provider migrates");
    let pg = cfg
        .storage
        .postgres
        .get("default")
        .expect("[storage.postgres.default] exists");
    assert_eq!(
        pg.db_url.as_deref(),
        Some("postgres://u:p@localhost/zc"),
        "V2 [storage.provider.config].db_url must land at V3 storage.postgres.default.db_url"
    );
    assert_eq!(pg.schema, "zc_schema");
    assert_eq!(pg.table, "memories");
    assert_eq!(pg.connect_timeout_secs, Some(42));
}

// ─────────────────────────────────────────────────────────────
// T11 — cron job id drop + alias-keyed cron
// ─────────────────────────────────────────────────────────────

#[test]
fn t11_cron_job_id_dropped_and_alias_keyed() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[cron]
enabled = true

[[cron.jobs]]
id = "morning_digest"
name = "Morning Digest"
job_type = "agent"
prompt = "Summarize unread messages"
enabled = true
schedule = { kind = "cron", expr = "0 7 * * *" }
"#;
    let cfg = migrate_to_current(raw).expect("V1 cron migrates");
    let job = cfg
        .cron
        .get("morning_digest")
        .expect("cron job alias derived from id");
    assert_eq!(job.name.as_deref(), Some("Morning Digest"));
    assert_eq!(job.prompt.as_deref(), Some("Summarize unread messages"));

    // V2 had `id: String` on CronJobDecl; V3 removed it. Assert via raw value.
    let raw_value: toml::Value = toml::from_str(
        &zeroclaw_config::migration::migrate_file(raw)
            .expect("migrate_file succeeds")
            .expect("migration ran"),
    )
    .expect("migrated TOML parses");
    let raw_job = raw_value
        .get("cron")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("morning_digest"))
        .and_then(toml::Value::as_table)
        .expect("cron.morning_digest in raw migrated TOML");
    assert!(
        !raw_job.contains_key("id"),
        "V2 CronJobDecl.id must be dropped during V2→V3 cron restructure"
    );
}

#[test]
fn t11_cron_subsystem_knobs_moved_to_scheduler() {
    let cfg = v3_config();
    assert_eq!(
        cfg.scheduler.max_run_history, 50,
        "V2 cron.max_run_history must move to scheduler.max_run_history"
    );
    assert!(
        cfg.scheduler.catch_up_on_startup,
        "V2 cron.catch_up_on_startup must move to scheduler.catch_up_on_startup"
    );
}

// ─────────────────────────────────────────────────────────────
// T12 — reliability fallback fields dropped
// ─────────────────────────────────────────────────────────────

#[test]
fn t12_reliability_fallback_fields_dropped() {
    let value = v3_value();
    let reliability = value
        .get("reliability")
        .and_then(toml::Value::as_table)
        .expect("[reliability] block survives with non-fallback fields");
    assert!(
        !reliability.contains_key("fallback_providers"),
        "V2 reliability.fallback_providers must be dropped (provider fallback eradicated)"
    );
    assert!(
        !reliability.contains_key("model_fallbacks"),
        "V2 reliability.model_fallbacks must be dropped"
    );
    // Unrelated fields stay (provider_retries was set in the fixture).
    assert!(
        reliability.contains_key("provider_retries"),
        "non-fallback reliability fields must survive"
    );
}

// ─────────────────────────────────────────────────────────────
// T13 — security.sandbox + .resources fold into risk_profiles.default
// ─────────────────────────────────────────────────────────────

#[test]
fn t13_security_sandbox_folded_into_risk_profile() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[autonomy]
level = "supervised"

[security.sandbox]
enabled = true
backend = "firejail"
firejail_args = ["--noroot"]
"#;
    let cfg = migrate_to_current(raw).expect("V1 security.sandbox migrates");
    let profile = cfg
        .risk_profiles
        .get("default")
        .expect("risk_profiles.default present");
    assert_eq!(
        profile.sandbox_enabled,
        Some(true),
        "V2 [security.sandbox].enabled must fold into risk_profiles.default.sandbox_enabled"
    );
    assert_eq!(
        profile.sandbox_backend.as_deref(),
        Some("firejail"),
        "V2 [security.sandbox].backend must fold into risk_profiles.default.sandbox_backend"
    );
    assert_eq!(
        profile.firejail_args,
        vec!["--noroot"],
        "V2 [security.sandbox].firejail_args must carry over"
    );
}

#[test]
fn t13_security_resources_dropped_during_migration() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[autonomy]
level = "supervised"

[security.resources]
max_memory_mb = 512
max_cpu_time_seconds = 600
max_subprocesses = 10
memory_monitoring = true
"#;
    // The block is dropped during V2→V3 migration: no V3 enforcement
    // codepath consumed these fields, sandbox backends own resource
    // budgets. Migration must still succeed and load a valid Config.
    let cfg = migrate_to_current(raw).expect("V1 security.resources migrates");
    let profile = cfg
        .risk_profiles
        .get("default")
        .expect("risk_profiles.default present");
    assert_eq!(profile.level, AutonomyLevel::Supervised);
}

// ─────────────────────────────────────────────────────────────
// T14 — per-agent V2→V3 transforms
// ─────────────────────────────────────────────────────────────

#[test]
fn t14a_max_iterations_renamed_to_max_tool_iterations() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.max_tool_iterations, 25,
        "V2 max_iterations=25 must land at V3 max_tool_iterations on the agent"
    );
}

#[test]
fn t14b_runtime_overrides_synthesize_per_agent_runtime_profile() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.runtime_profile, "agent_complex_agent",
        "V2 runtime overrides must point agent at synthesized per-agent runtime profile"
    );
    let runtime = cfg
        .runtime_profiles
        .get("agent_complex_agent")
        .expect("synthesized runtime_profiles.agent_complex_agent");
    assert!(runtime.agentic);
    let risk = cfg
        .risk_profiles
        .get("agent_complex_agent")
        .expect("synthesized risk_profiles.agent_complex_agent (allowed_tools home)");
    assert_eq!(risk.allowed_tools, vec!["shell", "memory"]);
}

#[test]
fn t14b2_per_agent_timeout_secs_folds_onto_model_provider_entry() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    let (provider_type, provider_alias) = agent
        .model_provider
        .split_once('.')
        .expect("agents.complex_agent.model_provider is <type>.<alias>");
    let entry = cfg
        .providers
        .models
        .find(provider_type, provider_alias)
        .expect("model_providers entry exists for complex_agent's brain");
    assert_eq!(
        entry.timeout_secs,
        Some(180),
        "V2 agent timeout_secs must land on the agent's model_provider, not runtime_profile"
    );
}

#[test]
fn t14c_max_depth_synthesizes_per_agent_runtime_profile() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.runtime_profile, "agent_complex_agent",
        "V2 max_depth must point agent at synthesized per-agent runtime profile"
    );
    let profile = cfg
        .runtime_profiles
        .get("agent_complex_agent")
        .expect("synthesized runtime_profiles.agent_complex_agent");
    assert_eq!(profile.max_delegation_depth, 4);
    assert_eq!(
        profile.agentic_timeout_secs,
        Some(600),
        "V2 agent agentic_timeout_secs must land on the agent's runtime_profile"
    );
}

#[test]
fn t14d_skills_directory_synthesizes_per_agent_skill_bundle() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    // skills_directory is gone from the agent and replaced with a
    // synthesized skill_bundle alias.
    assert!(
        agent
            .skill_bundles
            .iter()
            .any(|alias| alias == "agent_complex_agent"),
        "agents.complex_agent.skill_bundles must reference the synthesized \
         per-agent bundle alias; got {:?}",
        agent.skill_bundles
    );

    // The bundle entry exists, but the absolute V2 path is outside
    // <install>/shared/ — V3 drops it and the bundle resolves to the
    // default <install>/shared/skills/<alias>/ at runtime.
    let bundle = cfg
        .skill_bundles
        .get("agent_complex_agent")
        .expect("skill_bundles.agent_complex_agent synthesized from V2 skills_directory");
    assert_eq!(
        bundle.directory, None,
        "V2 skills_directory outside <install>/shared/ must drop to default, not carry the V2 path"
    );

    // skills_directory must not survive on the V3 agent (V3 schema has
    // no slot for it).
    let value = v3_value();
    let raw_agent = value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("complex_agent"))
        .and_then(toml::Value::as_table)
        .expect("agents.complex_agent in raw migrated TOML");
    assert!(
        !raw_agent.contains_key("skills_directory"),
        "V2 skills_directory field must be removed from the V3 agent block"
    );
}

// ─────────────────────────────────────────────────────────────
// V3 fields synthesized from V1/V2 input
// ─────────────────────────────────────────────────────────────

#[test]
fn autonomy_synthesized_into_risk_profiles_default() {
    let cfg = v3_config();
    let profile = cfg
        .risk_profiles
        .get("default")
        .expect("risk_profiles.default synthesized from [autonomy]");
    assert_eq!(profile.allowed_commands, vec!["ls", "git", "cat"]);
    assert!(profile.workspace_only);
    assert_eq!(
        profile.excluded_tools,
        vec!["browser"],
        "V2 non_cli_excluded_tools renamed to V3 excluded_tools during fold"
    );
    let runtime = cfg
        .runtime_profiles
        .get("default")
        .expect("runtime_profiles.default synthesized from [autonomy] budget fields");
    assert_eq!(runtime.shell_timeout_secs, 60);
}

#[test]
fn agent_synthesized_into_runtime_profiles_default() {
    let cfg = v3_config();
    let profile = cfg
        .runtime_profiles
        .get("default")
        .expect("runtime_profiles.default synthesized from [agent]");
    assert_eq!(profile.parallel_tools, Some(true));
    assert_eq!(profile.max_history_messages, Some(50));
    assert_eq!(profile.max_context_tokens, Some(32000));
    assert_eq!(profile.tool_dispatcher.as_deref(), Some("auto"));
}

// ─────────────────────────────────────────────────────────────
// cost.prices drop (per #5947 — composite V2 keys can't be remapped
// onto V3 alias-keyed paths without heuristics; operators paste
// manually under the right block).
// ─────────────────────────────────────────────────────────────

#[test]
fn cost_prices_dropped_not_folded() {
    let cfg = v3_config();
    let anth = cfg
        .providers
        .models
        .find("anthropic", "default")
        .expect("anthropic.default exists");
    assert!(
        anth.pricing.is_empty(),
        "V2 cost.prices entries must be dropped on migration; \
         got pricing entries on anthropic.default: {:?}",
        anth.pricing.keys().collect::<Vec<_>>()
    );

    // The migrated config also must not carry [cost.prices.*] anywhere.
    let value = v3_value();
    let cost_prices = value
        .get("cost")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("prices"));
    assert!(
        cost_prices.is_none(),
        "V3 [cost] block must not retain prices: {cost_prices:?}"
    );
}

// ─────────────────────────────────────────────────────────────
// passthrough + comment preservation
// ─────────────────────────────────────────────────────────────

#[test]
fn passthrough_propagates_unknown_section() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[my_custom_section]
custom_field = "preserved-through-chain"
nested_value = 42
"#;
    let migrated = zeroclaw_config::migration::migrate_file(raw)
        .expect("migrate_file succeeds")
        .expect("migration ran");
    let value: toml::Value = toml::from_str(&migrated).expect("migrated TOML parses");
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
    // [cost] survives V1 → V3 (with prices stripped). Its leading comment
    // should round-trip through the toml_edit::DocumentMut reconciliation.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

# Cost tracking limits
[cost]
enabled = true
daily_limit_usd = 10.0
"#;
    let migrated = migrate_file(raw)
        .expect("migrate_file succeeds")
        .expect("migration ran");
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
// discord_history bot_token conflict — per #5947, when the legacy
// [channels.discord-history].bot_token differs from
// [channels.discord].bot_token, the migration drops the history
// token (the discord token wins) and emits a WARN naming the source.
// Two-bot deployments must reconfigure manually.
// ─────────────────────────────────────────────────────────────

#[test]
fn discord_history_bot_token_conflict_drops_history_token() {
    // Both blocks present with different bot_tokens; discord wins.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.discord]
enabled = true
bot_token = "discord-token-survives"
guild_id = "11111"

[channels_config.discord_history]
enabled = true
bot_token = "history-token-dropped"
channel_ids = ["aaaa"]
"#;
    let cfg = migrate_to_current(raw).expect("migration succeeds despite bot_token conflict");
    let discord = cfg
        .channels
        .discord
        .get("default")
        .expect("merged channels.discord.default present");
    assert_eq!(
        discord.bot_token, "discord-token-survives",
        "the [channels.discord] bot_token must win over the dropped \
         [channels.discord-history] bot_token"
    );
    assert!(
        discord.archive,
        "the discord_history fold still flips archive=true on the merged block"
    );
}

// ─────────────────────────────────────────────────────────────
// Feishu rename — V3 collapses Feishu and Lark to one channel type.
// V2 [channels.feishu] becomes V3 [channels.lark.feishu] (alias name
// is "feishu", not "default") so two-bot deployments with both
// [channels.lark] AND [channels.feishu] survive as two distinct V3
// aliases without losing data.
// ─────────────────────────────────────────────────────────────

#[test]
fn feishu_only_block_folds_into_lark_feishu_alias() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.feishu]
enabled = true
app_id = "feishu_app_id_123"
app_secret = "feishu_secret"
mention_only = true
"#;
    let cfg = migrate_to_current(raw).expect("Feishu-only fold migration succeeds");
    assert!(
        cfg.channels.lark.contains_key("feishu"),
        "[channels.feishu] must surface as [channels.lark.feishu] after fold"
    );
    assert!(
        !cfg.channels.lark.contains_key("default"),
        "no spurious lark.default alias when only [channels.feishu] was set"
    );
    let lark = &cfg.channels.lark["feishu"];
    assert!(
        lark.use_feishu,
        "use_feishu must be set true on the folded entry so the runtime routes to open.feishu.cn"
    );
    assert_eq!(lark.app_id, "feishu_app_id_123");
    assert!(lark.mention_only);
}

#[test]
fn feishu_and_lark_blocks_become_two_aliases() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.lark]
enabled = true
app_id = "lark_intl_app"
app_secret = "lark_secret"

[channels_config.feishu]
enabled = true
app_id = "feishu_cn_app"
app_secret = "feishu_secret"
encrypt_key = "feishu_encrypt"
"#;
    let cfg = migrate_to_current(raw).expect("two-bot migration succeeds without drops");

    let lark_default = cfg
        .channels
        .lark
        .get("default")
        .expect("lark.default carries the V2 [channels.lark] block");
    assert_eq!(lark_default.app_id, "lark_intl_app");
    assert!(
        !lark_default.use_feishu,
        "lark.default keeps Lark international routing"
    );

    let lark_feishu = cfg
        .channels
        .lark
        .get("feishu")
        .expect("lark.feishu carries the V2 [channels.feishu] block");
    assert_eq!(lark_feishu.app_id, "feishu_cn_app");
    assert!(
        lark_feishu.use_feishu,
        "lark.feishu routes to Feishu (open.feishu.cn) via use_feishu = true"
    );
    assert_eq!(lark_feishu.encrypt_key.as_deref(), Some("feishu_encrypt"));
}

#[test]
fn feishu_block_with_same_app_id_as_lark_still_lands_under_feishu_alias() {
    // Even when both blocks share an app_id (uncommon — operator double-
    // configured the same bot), the migration preserves both rows. The
    // operator can dedupe post-migration with full visibility; the
    // migration never silently merges or drops.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.lark]
enabled = true
app_id = "shared_app_id"
app_secret = "lark_secret"

[channels_config.feishu]
enabled = true
app_id = "shared_app_id"
app_secret = "feishu_secret"
encrypt_key = "feishu_encrypt"
"#;
    let cfg = migrate_to_current(raw).expect("same-app_id migration preserves both aliases");
    assert_eq!(cfg.channels.lark["default"].app_id, "shared_app_id");
    assert!(!cfg.channels.lark["default"].use_feishu);
    assert_eq!(cfg.channels.lark["feishu"].app_id, "shared_app_id");
    assert!(cfg.channels.lark["feishu"].use_feishu);
    assert_eq!(
        cfg.channels.lark["feishu"].encrypt_key.as_deref(),
        Some("feishu_encrypt")
    );
}

// ─────────────────────────────────────────────────────────────
// V1/V2 colon-URL provider strings — `(custom|anthropic-custom):<url>`.
// Pre-fix the migration used the raw colon-URL string as the V3 outer
// provider key, then synthesized `model_provider = "<type>:<url>.<alias>"`.
// V3's `split_once('.')` resolution then tokenized at the first URL dot
// (e.g. inside `api.z.ai`), making the reference unresolvable. The fix
// splits the URL into `uri` on the alias entry and uses only the
// prefix as the V3 type key, keeping `<type>.<alias>` parseable.
// ─────────────────────────────────────────────────────────────

#[test]
fn anthropic_custom_colon_url_default_provider_folds_under_anthropic() {
    // Phase 8 migration sweep: V2 `anthropic-custom:URL` form folds under
    // model_providers.anthropic.custom with the URL split out onto the
    // alias entry's `uri` field.
    let raw = r#"
default_provider = "anthropic-custom:https://api.z.ai/api/anthropic"
default_model = "claude-sonnet-4"
api_key = "sk-zai-test"
"#;
    let cfg =
        migrate_to_current(raw).expect("migration succeeds despite colon-URL default_provider");
    let entry = cfg
        .providers
        .models
        .find("anthropic", "custom")
        .expect("V2 anthropic-custom synonym must fold under model_providers.anthropic.custom");
    assert_eq!(
        entry.uri.as_deref(),
        Some("https://api.z.ai/api/anthropic"),
        "the URL portion of the V2 colon-URL form must land in uri on the alias entry"
    );
    assert_eq!(entry.model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(entry.api_key.as_deref(), Some("sk-zai-test"));
}

#[test]
fn custom_colon_url_default_provider_splits_into_uri() {
    let raw = r#"
default_provider = "custom:http://localhost:8080/v1"
default_model = "local-model"
"#;
    let cfg =
        migrate_to_current(raw).expect("migration succeeds for plain `custom:` colon-URL form");
    let entry = cfg
        .providers
        .models
        .find("custom", "default")
        .expect("V3 outer key must be `custom`, not the raw colon-URL");
    assert_eq!(
        entry.uri.as_deref(),
        Some("http://localhost:8080/v1"),
        "the URL portion of the V2 colon-URL form must land in uri"
    );
    assert_eq!(entry.model.as_deref(), Some("local-model"));
}

#[test]
fn agent_inline_brain_colon_url_provider_splits_into_uri() {
    // Per-agent colon-URL: synthesize_agent_brains used the raw string as
    // the V3 outer provider key. Same dot-bearing-key bug — must split.
    let raw = r#"
schema_version = 2

[agents.researcher]
provider = "anthropic-custom:https://api.z.ai/api/anthropic"
model = "claude-opus-4"
api_key = "sk-zai-agent"
"#;
    let v3 = migrate_v2(raw);
    let agent = v3
        .get("agents")
        .and_then(|v| v.get("researcher"))
        .expect("agents.researcher present");
    let model_provider = agent
        .get("model_provider")
        .and_then(|v| v.as_str())
        .expect("model_provider reference is a string");
    let (type_key, alias_key) = model_provider
        .split_once('.')
        .expect("model_provider must split cleanly on the type/alias dot");
    assert_eq!(
        type_key, "anthropic-custom",
        "type segment must be the dot-free prefix, not the colon-URL form"
    );
    assert_eq!(alias_key, "agent_researcher");
    assert!(
        !alias_key.contains('/'),
        "the URL must not bleed into the alias segment, got alias `{alias_key}`"
    );

    let synthesized = lookup_dotted(&v3, "providers.models.anthropic-custom.agent_researcher")
        .expect("providers.models.anthropic-custom.agent_researcher synthesized");
    assert_eq!(
        synthesized.get("uri").and_then(toml::Value::as_str),
        Some("https://api.z.ai/api/anthropic"),
        "the colon-URL's URL portion must land in uri on the synthesized alias entry"
    );
    assert_eq!(
        synthesized.get("model").and_then(toml::Value::as_str),
        Some("claude-opus-4")
    );
    assert_eq!(
        synthesized.get("api_key").and_then(toml::Value::as_str),
        Some("sk-zai-agent")
    );
}

// ─────────────────────────────────────────────────────────────
// signal "dm" sentinel — separate test because the V1 fixture above
// uses a non-"dm" value to exercise the array fold path. This test
// inlines a minimal V1 input to exercise the sentinel branch.
// ─────────────────────────────────────────────────────────────

#[test]
fn t6_signal_dm_sentinel_sets_dm_only() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.signal]
enabled = true
http_url = "http://127.0.0.1:8686"
account = "+15555550100"
group_id = "dm"
"#;
    let cfg = migrate_to_current(raw).expect("dm-sentinel V1 migrates");
    let signal = cfg
        .channels
        .signal
        .get("default")
        .expect("channels.signal.default present");
    assert!(
        signal.dm_only,
        "V2 signal.group_id=\"dm\" must set V3 signal.dm_only=true"
    );
    assert!(
        signal.group_ids.is_empty(),
        "the \"dm\" sentinel must NOT also land in group_ids[]"
    );
}

// ─────────────────────────────────────────────────────────────
// model_routes / embedding_routes — V2 spelled the routing target
// as `provider`, V3 as `model_provider`. The runtime serde alias was
// removed; the rename has to happen at migration time.
// ─────────────────────────────────────────────────────────────

#[test]
fn v2_model_routes_provider_field_renamed_to_model_provider() {
    let raw = r#"
schema_version = 2

[providers]
default_provider = "openai"
default_model = "gpt-4o"

[[model_routes]]
hint = "vision"
provider = "openai"
model = "gpt-4-vision"

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.1-8b-instant"

[agents.default]
model_provider = "openai.default"
"#;
    let cfg = migrate_to_current(raw).expect("V2 model_routes migrate");
    let vision = cfg
        .model_routes
        .iter()
        .find(|r| r.hint == "vision")
        .expect("vision route");
    assert_eq!(
        vision.model_provider, "openai.default",
        "bare V2 `provider = openai` must be promoted to dotted V3 `openai.default`"
    );
    assert_eq!(vision.model, "gpt-4-vision");

    let fast = cfg
        .model_routes
        .iter()
        .find(|r| r.hint == "fast")
        .expect("fast route");
    assert_eq!(fast.model_provider, "groq.default");
    assert_eq!(fast.model, "llama-3.1-8b-instant");
}

#[test]
fn v2_embedding_routes_provider_field_renamed_to_model_provider() {
    let raw = r#"
schema_version = 2

[providers]
default_provider = "openai"
default_model = "gpt-4o"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536

[agents.default]
model_provider = "openai.default"
"#;
    let cfg = migrate_to_current(raw).expect("V2 embedding_routes migrate");
    let semantic = cfg
        .embedding_routes
        .iter()
        .find(|r| r.hint == "semantic")
        .expect("semantic route");
    assert_eq!(
        semantic.model_provider, "openai.default",
        "bare V2 `provider = openai` must be promoted to dotted V3 `openai.default`"
    );
    assert_eq!(semantic.model, "text-embedding-3-small");
    assert_eq!(semantic.dimensions, Some(1536));
}

#[test]
fn v2_route_rename_idempotent_when_already_v3() {
    // An operator who already wrote `model_provider` directly (or migration
    // ran twice) must end up with the V3 field unchanged and no stray
    // `provider` key floating around.
    let raw = r#"
schema_version = 2

[providers]
default_provider = "openai"
default_model = "gpt-4o"

[[model_routes]]
hint = "vision"
model_provider = "openai"
model = "gpt-4-vision"

[agents.default]
model_provider = "openai.default"
"#;
    let cfg = migrate_to_current(raw).expect("idempotent V3-shaped routes migrate");
    let vision = cfg
        .model_routes
        .iter()
        .find(|r| r.hint == "vision")
        .expect("vision route");
    assert_eq!(
        vision.model_provider, "openai.default",
        "bare model_provider value is promoted to dotted form regardless of source field name"
    );
}

// ─────────────────────────────────────────────────────────────
// Channel allowed_users → peer_groups synthesis
// ─────────────────────────────────────────────────────────────

#[test]
fn v2_channel_allowed_users_fold_into_synthesized_peer_groups() {
    let v3 = migrate_v2(
        r#"
[channels.discord]
enabled = true
bot_token = "tok"
allowed_users = ["alice", "bob"]

[channels.slack]
enabled = true
bot_token = "tok"
allowed_users = ["@oncall"]
"#,
    );
    let groups = v3
        .get("peer_groups")
        .and_then(toml::Value::as_table)
        .expect("peer_groups synthesized");

    let discord_group = groups
        .get("discord_default")
        .and_then(toml::Value::as_table)
        .expect("discord allow-list folds into [peer_groups.discord_default]");
    assert_eq!(
        discord_group.get("channel").and_then(toml::Value::as_str),
        Some("discord"),
    );
    let discord_peers: Vec<&str> = discord_group
        .get("external_peers")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(discord_peers, vec!["alice", "bob"]);

    let slack_group = groups
        .get("slack_default")
        .and_then(toml::Value::as_table)
        .expect("slack allow-list folds into [peer_groups.slack_default]");
    assert_eq!(
        slack_group.get("channel").and_then(toml::Value::as_str),
        Some("slack"),
    );
}

#[test]
fn v2_channel_allowed_users_fold_skips_wildcard_only_lists() {
    // `allowed_users = ["*"]` means "anyone"; a peer group can't
    // express that, so the synthesis is a no-op for those channels.
    let v3 = migrate_v2(
        r#"
[channels.telegram]
enabled = true
bot_token = "tok"
allowed_users = ["*"]
"#,
    );
    assert!(
        v3.get("peer_groups").is_none()
            || v3
                .get("peer_groups")
                .and_then(toml::Value::as_table)
                .map(|t| !t.contains_key("telegram_default"))
                .unwrap_or(true),
        "wildcard-only allowed_users must not synthesize a peer group"
    );
}

#[test]
fn v2_channel_allowed_users_fold_does_not_overwrite_authored_peer_group() {
    // If the operator already authored a peer_group with the
    // synthesized name, leave it alone — silent overwrite would lose
    // their hand-curated `agents` list.
    let v3 = migrate_v2(
        r#"
[channels.discord]
enabled = true
bot_token = "tok"
allowed_users = ["alice"]

[peer_groups.discord_default]
channel = "discord.default"
agents = ["researcher"]
"#,
    );
    let group = v3
        .get("peer_groups")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("discord_default"))
        .and_then(toml::Value::as_table)
        .expect("authored group survives");
    let agents: Vec<&str> = group
        .get("agents")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(agents, vec!["researcher"]);
    // The authored group has no `external_peers` field; the synthesizer
    // must not have injected one.
    assert!(group.get("external_peers").is_none());
}

/// Per-channel-type peer-auth field name regression. The V2 field
/// name varied per platform (allowed_users, allowed_contacts,
/// allowed_from, allowed_numbers, allowed_senders, allowed_pubkeys);
/// every one of them folds into `external_peers` on a synthesized
/// peer group and the original channel field is REMOVED.
#[test]
fn v2_every_inbound_peer_field_folds_and_is_stripped() {
    let v3 = migrate_v2(
        r#"
[channels.imessage]
enabled = true
allowed_contacts = ["+15551234567"]

[channels.signal]
enabled = true
http_url = "http://localhost:8080"
account = "+15555550100"
allowed_from = ["+15551234567"]

[channels.whatsapp]
enabled = true
phone_number_id = "id"
business_account_id = "acct"
access_token = "tok"
verify_token = "v"
allowed_numbers = ["+15551234567"]

[channels.linq]
enabled = true
api_token = "linq-tok"
from_phone = "+15555550100"
allowed_senders = ["+15551234567"]

[channels.nostr]
enabled = true
relay_url = "wss://relay.example"
private_key = "nsec1xxx"
allowed_pubkeys = ["npub1abc"]

[channels.email]
enabled = true
provider = "imap"
imap_host = "imap.example"
imap_port = 993
smtp_host = "smtp.example"
smtp_port = 587
username = "bot@example"
password = "p"
from_address = "bot@example"
allowed_senders = ["ops@example"]
"#,
    );

    // Helper: assert the synthesized peer group has the expected
    // username, and that the original field is no longer present on
    // the channel block.
    let pg = v3
        .get("peer_groups")
        .and_then(toml::Value::as_table)
        .expect("peer_groups synthesized");
    let channels = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .expect("channels exists");

    for (channel_type, field_name, expected_user) in [
        ("imessage", "allowed_contacts", "+15551234567"),
        ("signal", "allowed_from", "+15551234567"),
        ("whatsapp", "allowed_numbers", "+15551234567"),
        ("linq", "allowed_senders", "+15551234567"),
        ("nostr", "allowed_pubkeys", "npub1abc"),
        ("email", "allowed_senders", "ops@example"),
    ] {
        let group_name = format!("{channel_type}_default");
        let group = pg
            .get(&group_name)
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("peer_groups.{group_name} synthesized"));
        let peer = group
            .get("external_peers")
            .and_then(toml::Value::as_array)
            .unwrap()
            .first()
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("external_peers[0] for {channel_type}"));
        assert_eq!(peer, expected_user, "{channel_type} username");

        let channel_alias = channels
            .get(channel_type)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("default"))
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("channels.{channel_type}.default present"));
        assert!(
            channel_alias.get(field_name).is_none(),
            "channels.{channel_type}.default.{field_name} must be stripped after fold"
        );
    }
}

#[test]
fn v2_matrix_allowed_users_folds_and_allowed_rooms_stays() {
    let v3 = migrate_v2(
        r#"
[channels.matrix]
enabled = true
homeserver = "https://matrix.org"
access_token = "tok"
allowed_users = ["@alice:matrix.org", "@bob:matrix.org"]
allowed_rooms = ["!ops:matrix.org"]
"#,
    );

    // 1. Synthesized peer group with the channel ref and MXIDs.
    let group = v3
        .get("peer_groups")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("matrix_default"))
        .and_then(toml::Value::as_table)
        .expect("matrix allow-list folds into [peer_groups.matrix_default]");
    let peers: Vec<&str> = group
        .get("external_peers")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(peers, vec!["@alice:matrix.org", "@bob:matrix.org"]);

    // 2. allowed_users is REMOVED from the channel block — peer
    //    authorization is in peer_groups only.
    let matrix = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("matrix"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .expect("channels.matrix.default present after V2→V3");
    assert!(
        matrix.get("allowed_users").is_none(),
        "channel-level allowed_users must be stripped after the fold"
    );

    // 3. allowed_rooms is container scope (rooms aren't peers); stays.
    let rooms: Vec<&str> = matrix
        .get("allowed_rooms")
        .and_then(toml::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(rooms, vec!["!ops:matrix.org"]);
}

// ─────────────────────────────────────────────────────────────
// V3_CHANNEL_TYPES coverage — every typed nested channel slot on
// `ChannelsConfig` must appear in the migration walker's alias-wrap
// list. Missing entries silently slip through the "unmodeled keys
// passthrough" branch and surface as type errors at V3 deserialize
// time (a V2 `[channels.foo] enabled = false` block remains flat,
// then deserialize tries to read it as `HashMap<String, FooConfig>`
// and panics with `invalid type: boolean false, expected struct
// FooConfig`). The user report this regression test came from is
// at the bottom of the next test.
// ─────────────────────────────────────────────────────────────

#[test]
fn v2_channels_voice_duplex_block_alias_wraps() {
    // Reproduces the user-reported migration error in v0.8.0:
    //   invalid type: boolean `false`, expected struct VoiceDuplexConfig
    //   in `channels.voice_duplex.enabled`
    // Cause: voice_duplex was missing from V3_CHANNEL_TYPES and went
    // through the unmodeled-keys passthrough, leaving the V2 block flat.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.voice_duplex]
enabled = false
"#;
    let cfg = migrate_to_current(raw)
        .expect("voice_duplex flat V2 block must alias-wrap, not deserialize as flat struct");
    // enabled = false drops the block entirely (T7 enabled-keep filter),
    // so we just assert the migration succeeded.
    assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn v2_channels_voice_wake_block_alias_wraps() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.voice_wake]
enabled = false
"#;
    let cfg = migrate_to_current(raw).expect("voice_wake flat V2 block must alias-wrap");
    assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn v2_channels_mqtt_block_alias_wraps() {
    // V3 preserves disabled channel blocks rather than dropping them, so
    // the test config has to satisfy MqttConfig's required `broker_url` /
    // `client_id` fields — a parked channel still has to deserialize.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[channels_config.mqtt]
enabled = false
broker_url = "mqtt://localhost:1883"
client_id = "parked-test-client"
"#;
    let cfg = migrate_to_current(raw).expect("mqtt flat V2 block must alias-wrap");
    assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
    let mqtt = cfg
        .channels
        .mqtt
        .get("default")
        .expect("parked mqtt block survives V2→V3 migration");
    assert!(
        !mqtt.enabled,
        "operator's enabled = false ports through verbatim"
    );
}

#[test]
fn v2_tunnel_provider_renamed_to_tunnel_provider() {
    // V2 grammar wrote `[tunnel] provider = "cloudflare"`. V3 qualifies the
    // field name as `tunnel_provider` (it's not a model provider). Without
    // this rename V3 deserialize fails with `missing field tunnel_provider`.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[tunnel]
provider = "cloudflare"

[tunnel.cloudflare]
token = "stub"
"#;
    let cfg = migrate_to_current(raw).expect("V2 [tunnel] provider must migrate");
    assert_eq!(cfg.tunnel.tunnel_provider, "cloudflare");
}

#[test]
fn v2_tunnel_provider_none_migrates() {
    // The exact shape from the user-reported config: `[tunnel] provider =
    // "none"` with empty sub-blocks.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[tunnel]
provider = "none"

[tunnel.cloudflare]
token = ""

[tunnel.custom]
start_command = ""

[tunnel.tailscale]
funnel = false
"#;
    let cfg = migrate_to_current(raw).expect("V2 [tunnel] provider = \"none\" must migrate");
    assert_eq!(cfg.tunnel.tunnel_provider, "none");
}

#[test]
fn v2_web_search_provider_renamed_to_search_provider() {
    // V2 grammar wrote `[web_search] provider = "duckduckgo"`. V3 qualifies
    // as `search_provider` (it's not a model provider).
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[web_search]
enabled = true
provider = "duckduckgo"
max_results = 5
"#;
    let cfg = migrate_to_current(raw).expect("V2 [web_search] provider must migrate");
    assert_eq!(cfg.web_search.search_provider, "duckduckgo");
}

#[test]
fn rename_subkey_drops_v2_form_when_operator_already_wrote_v3_form() {
    // Defensive: an operator who hand-edited their config to use the V3
    // qualified key but left the V2 stale key behind should not double-error
    // — V3 wins, V2 is dropped silently.
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[tunnel]
provider = "cloudflare"
tunnel_provider = "tailscale"

[tunnel.tailscale]
funnel = true
"#;
    let cfg = migrate_to_current(raw).expect("V3-key-wins semantics");
    assert_eq!(cfg.tunnel.tunnel_provider, "tailscale");
}

#[test]
fn v3_channel_types_covers_every_typed_channel_slot() {
    // Drift gate: every `#[nested] HashMap<String, T>` field under
    // ChannelsConfig must appear in V3_CHANNEL_TYPES (or be intentionally
    // folded into a sibling type — today only `feishu` qualifies, since
    // V2 `[channels.feishu]` is migrated to `[channels.lark.feishu]`).
    use std::collections::HashSet;
    use zeroclaw_config::schema::Config;
    use zeroclaw_config::schema::v2::V3_CHANNEL_TYPES;

    let listed: HashSet<&str> = V3_CHANNEL_TYPES.iter().copied().collect();

    // Channel types that are intentionally NOT in V3_CHANNEL_TYPES:
    // they're folded into another channel type by a dedicated walker
    // step that runs before the alias-wrap loop.
    let folded_into_sibling: HashSet<&str> = ["feishu"].into_iter().collect();

    // map_key_sections paths come from the per-struct `#[prefix = ...]`
    // attribute, which historically uses kebab-case for multi-word slots
    // (`channels.gmail-push`). The migration walker compares against the
    // TOML key, which is the snake-case field name (`channels.gmail_push`).
    // Normalize before comparing so the two never silently disagree on
    // separator choice.
    let typed_channel_slots: Vec<String> = Config::map_key_sections()
        .into_iter()
        .filter_map(|s| {
            let mut parts = s.path.splitn(2, '.');
            match (parts.next(), parts.next()) {
                (Some("channels"), Some(rest)) if !rest.contains('.') => {
                    Some(rest.replace('-', "_"))
                }
                _ => None,
            }
        })
        .collect();

    let mut missing: Vec<&String> = typed_channel_slots
        .iter()
        .filter(|slot| !listed.contains(slot.as_str()))
        .filter(|slot| !folded_into_sibling.contains(slot.as_str()))
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "ChannelsConfig has typed channel slots that V3_CHANNEL_TYPES does \
         not alias-wrap during V2→V3 migration. Add each missing entry to \
         the const in `crates/zeroclaw-config/src/schema/v2.rs`, or add a \
         dedicated fold step before the alias-wrap loop and list the slot \
         under `folded_into_sibling` here.\n\nMissing slots: {:?}",
        missing
    );
}

// ─────────────────────────────────────────────────────────────
// V2 [heartbeat] enabled → V3 heartbeat.agent backfill
// ─────────────────────────────────────────────────────────────

#[test]
fn v2_heartbeat_enabled_backfills_agent_to_synthesized_default() {
    // V2 heartbeat had no `agent` field; V3 requires one when enabled.
    // Migration should fill `agent = "default"` when the agents fold
    // synthesizes `agents.default` from an implicit single-agent V2 config.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[providers.models.openai]
api_key = "sk-test"
model = "gpt-4o"

[heartbeat]
enabled = true
"#,
    );

    assert_eq!(
        v3.get("heartbeat")
            .and_then(|h| h.get("agent"))
            .and_then(toml::Value::as_str),
        Some("default"),
        "heartbeat.agent should be backfilled to the synthesized default agent"
    );
}

#[test]
fn v2_heartbeat_enabled_preserves_explicit_agent() {
    // Operator-set agent wins — backfill must be additive only.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[providers.models.openai]
api_key = "sk-test"
model = "gpt-4o"

[heartbeat]
enabled = true
agent = "watcher"
"#,
    );

    assert_eq!(
        v3.get("heartbeat")
            .and_then(|h| h.get("agent"))
            .and_then(toml::Value::as_str),
        Some("watcher"),
        "explicit heartbeat.agent must survive migration"
    );
}

#[test]
fn v2_heartbeat_disabled_skips_backfill() {
    // When disabled, leave heartbeat.agent alone so the operator can
    // toggle `enabled = true` later without a stale alias appearing.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[providers.models.openai]
api_key = "sk-test"
model = "gpt-4o"

[heartbeat]
enabled = false
"#,
    );

    assert!(
        v3.get("heartbeat")
            .and_then(|h| h.get("agent"))
            .and_then(toml::Value::as_str)
            .is_none_or(str::is_empty),
        "heartbeat.agent must stay empty when heartbeat is disabled"
    );
}

// ─────────────────────────────────────────────────────────────
// `zeroclaw config generate <version>` end-to-end regression
// guards.
//
// These tests run the same `generate()` function the CLI invokes,
// then push the output through the typed migration chain and the
// V3 schema validator. A break in any of the following surfaces
// fails one of these tests:
//
// - V1Config / V2Config typed lens drifts away from real V1/V2 TOML
// - V2→V3 migration starts dropping or mistyping a section
// - A new required V3 schema field lands without a default and
//   without a corresponding migration synthesis step
// - V3 `Config::validate()` grows a new check that the V1 fixture
//   doesn't satisfy
// - `encrypt_secret_strings` stops covering a known secret key name
//
// Lower bounds (section counts, presence of named sections) are
// preferred over exact equality so adding new sections or aliases
// doesn't break the suite — only removals / regressions do.
// ─────────────────────────────────────────────────────────────

#[test]
fn generate_every_version_migrates_and_validates() {
    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(target, &GenerateOptions::default())
            .unwrap_or_else(|e| panic!("generate({target}) failed: {e:#}"));
        let cfg = migrate_to_current(&raw).unwrap_or_else(|e| {
            panic!("generate({target}) output failed to migrate to current schema: {e:#}")
        });
        // Validation rejects dangling references and structural mismatches.
        // A green load here means the typed chain plus the V3 validator
        // accept the generated config end-to-end. We tolerate `Err` only
        // when validate() surfaces a known-by-design fixture artifact
        // (the V1 fixture intentionally has an empty
        // `[model_providers.claude-code]` block, which Config::validate
        // does NOT reject — it just warns at load time).
        cfg.validate()
            .unwrap_or_else(|e| panic!("generate({target}) output fails Config::validate: {e:#}"));
    }
}

#[test]
fn generate_current_emits_at_current_schema_version() {
    let raw = generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
        .expect("generate current succeeds");
    let parsed: toml::Value = toml::from_str(&raw).expect("generated output parses as TOML");
    assert_eq!(
        parsed
            .get("schema_version")
            .and_then(toml::Value::as_integer),
        Some(i64::from(CURRENT_SCHEMA_VERSION)),
        "generate(CURRENT) must stamp the current schema_version"
    );
}

#[test]
fn generate_v1_is_v1_shape() {
    let raw = generate(1, &GenerateOptions::default()).expect("generate v1 succeeds");
    let parsed: toml::Value = toml::from_str(&raw).expect("v1 parses");
    let table = parsed.as_table().expect("root is a table");
    // V1 had no `schema_version` key — the absence is how V1 is detected.
    assert!(
        !table.contains_key("schema_version"),
        "V1 output must not carry a schema_version key (V1 predates the field)"
    );
    // V1 lives in `channels_config`; V2 renames to `channels`.
    assert!(
        table.contains_key("channels_config"),
        "V1 output uses the V1 channel-section name `channels_config`"
    );
    assert!(
        !table.contains_key("channels"),
        "V1 output must not carry the V2+ `channels` name yet"
    );
    // V1 stores provider entries flat at `[model_providers.<id>]`.
    let mp = table
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .expect("V1 has [model_providers] at top level");
    assert!(
        mp.contains_key("anthropic"),
        "V1 model_providers carries the anthropic entry"
    );
}

#[test]
fn generate_v2_is_v2_shape() {
    let raw = generate(2, &GenerateOptions::default()).expect("generate v2 succeeds");
    let parsed: toml::Value = toml::from_str(&raw).expect("v2 parses");
    let table = parsed.as_table().expect("root is a table");
    assert_eq!(
        table
            .get("schema_version")
            .and_then(toml::Value::as_integer),
        Some(2),
        "V2 output stamps schema_version = 2"
    );
    // V2 renamed channels_config → channels.
    assert!(
        table.contains_key("channels"),
        "V2 output uses `channels` (renamed from V1 `channels_config`)"
    );
    assert!(
        !table.contains_key("channels_config"),
        "V2 must not carry the V1 channel-section name"
    );
    // V2 nests model_providers under `[providers.models]`; V3 hoists them.
    let providers = table
        .get("providers")
        .and_then(toml::Value::as_table)
        .expect("V2 has top-level [providers] block");
    assert!(
        providers.contains_key("models"),
        "V2 nests provider entries under providers.models"
    );
    assert!(
        !table.contains_key("model_providers"),
        "V2 has not hoisted model_providers to top level yet (V3 does that)"
    );
}

#[test]
fn generate_v3_covers_every_v3_top_level_section() {
    // Lower-bound presence check: every section listed here is one the
    // embedded V1 fixture exercises end-to-end through `generate(3)`.
    // Adding new sections to the fixture is fine — only removing one of
    // these (or breaking its migration) fails.
    let cfg = migrate_to_current(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated current schema migrates");

    assert!(
        cfg.agents.contains_key("simple_agent"),
        "agents.simple_agent synthesized from V1 inline-brain agent"
    );
    assert!(
        cfg.agents.contains_key("complex_agent"),
        "agents.complex_agent synthesized from V1 inline-brain agent"
    );
    assert!(
        cfg.risk_profiles.contains_key("default"),
        "[autonomy] migrated to [risk_profiles.default]"
    );
    assert!(
        cfg.runtime_profiles.contains_key("default"),
        "[agent] migrated to [runtime_profiles.default]"
    );
    assert!(
        cfg.cron.contains_key("morning_briefing"),
        "V1 cron job migrated from [[cron.jobs]] to [cron.<alias>] keyed entry"
    );
    assert!(
        cfg.scheduler.enabled,
        "[scheduler] populated from V1 [cron] subsystem knobs"
    );
    assert!(
        !cfg.storage.qdrant.is_empty(),
        "[memory.qdrant] promoted to [storage.qdrant.default]"
    );
    assert!(
        !cfg.peer_groups.is_empty(),
        "channel allow-list fields fold into peer_groups during V2→V3"
    );
    // Comprehensive-fixture markers: each of these is a top-level V3
    // section the fixture covers. Drop one and the regression suite
    // surfaces it.
    assert!(cfg.gateway.require_pairing, "gateway block carried through");
    assert!(cfg.backup.enabled, "backup block carried through");
    assert!(cfg.heartbeat.enabled, "heartbeat block carried through");
    assert!(cfg.web_search.enabled, "web_search block carried through");
}

#[test]
fn generate_v3_channel_breadth_lower_bound() {
    // The V1 fixture covers a wide channel surface. Lower-bound count
    // catches accidental loss of a whole channel during migration.
    // Raise the bound only when adding more channels to the fixture.
    const MIN_CHANNEL_ALIASES: usize = 25;

    let cfg = migrate_to_current(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 migrates");

    let alias_count = cfg.channels.telegram.len()
        + cfg.channels.discord.len()
        + cfg.channels.slack.len()
        + cfg.channels.mattermost.len()
        + cfg.channels.webhook.len()
        + cfg.channels.imessage.len()
        + cfg.channels.matrix.len()
        + cfg.channels.signal.len()
        + cfg.channels.whatsapp.len()
        + cfg.channels.linq.len()
        + cfg.channels.wati.len()
        + cfg.channels.nextcloud_talk.len()
        + cfg.channels.mqtt.len()
        + cfg.channels.irc.len()
        + cfg.channels.lark.len()
        + cfg.channels.line.len()
        + cfg.channels.dingtalk.len()
        + cfg.channels.wecom.len()
        + cfg.channels.wechat.len()
        + cfg.channels.qq.len()
        + cfg.channels.twitter.len()
        + cfg.channels.mochat.len()
        + cfg.channels.reddit.len()
        + cfg.channels.bluesky.len()
        + cfg.channels.email.len()
        + cfg.channels.gmail_push.len()
        + cfg.channels.clawdtalk.len()
        + cfg.channels.voice_call.len();

    assert!(
        alias_count >= MIN_CHANNEL_ALIASES,
        "generate(V3) channel breadth dropped: expected ≥ {MIN_CHANNEL_ALIASES} \
         channel aliases across all types, got {alias_count}. Most likely \
         cause: a V1 channel block got silently dropped during migration."
    );
}

#[test]
fn generate_with_encrypt_produces_enc2_ciphertext_at_every_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SecretStore::new(tmp.path(), true);

    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(
            target,
            &GenerateOptions {
                encrypt_secrets: true,
                secret_store_dir: Some(tmp.path()),
            },
        )
        .unwrap_or_else(|e| panic!("generate({target}) --encrypt failed: {e:#}"));

        // The output must contain enc2: ciphertext (at least one secret
        // was encrypted) and must not contain any of the well-known
        // plaintext fixture secrets.
        assert!(
            raw.contains("enc2:"),
            "generate({target}) --encrypt produced no enc2: ciphertext"
        );
        for plaintext in &[
            "sk-v1-test-global",
            "matrix-bot-token",
            "discord-bot-token-v1",
            "telegram-bot-token-v1",
            "bsky-app-password",
            "qdrant-api-key",
            "pg-password",
        ] {
            assert!(
                !raw.contains(plaintext),
                "generate({target}) --encrypt leaked plaintext secret {plaintext:?}"
            );
        }

        // Round-trip a known leaf back through decrypt to prove the
        // ciphertext is real (not just a literal `enc2:` prefix).
        let parsed: toml::Value = toml::from_str(&raw).expect("encrypted output parses");
        let api_key_ciphertext = find_first_string_at_key(&parsed, "api_key")
            .unwrap_or_else(|| panic!("generate({target}) has no api_key leaf to decrypt"));
        assert!(
            api_key_ciphertext.starts_with("enc2:"),
            "generate({target}) api_key leaf must be enc2: ciphertext, got {api_key_ciphertext:?}"
        );
        let plaintext = store
            .decrypt(&api_key_ciphertext)
            .unwrap_or_else(|e| panic!("generate({target}) api_key failed to decrypt: {e:#}"));
        assert!(
            !plaintext.is_empty(),
            "generate({target}) api_key decrypted to empty string"
        );
    }
}

/// Walk a `toml::Value` and return the first string leaf whose key
/// matches `key`. Helper for encrypt round-trip assertions.
fn find_first_string_at_key(value: &toml::Value, key: &str) -> Option<String> {
    match value {
        toml::Value::Table(t) => {
            if let Some(toml::Value::String(s)) = t.get(key) {
                return Some(s.clone());
            }
            for child in t.values() {
                if let Some(found) = find_first_string_at_key(child, key) {
                    return Some(found);
                }
            }
            None
        }
        toml::Value::Array(items) => items.iter().find_map(|v| find_first_string_at_key(v, key)),
        _ => None,
    }
}

#[test]
fn encryption_covers_every_schema_secret_field() {
    // The encrypt walker derives its key-name allowlist from the typed
    // schema via Config::prop_fields().filter(is_secret). This test
    // proves end-to-end coverage by:
    //
    //   1. Generating a comprehensive V3 config from the V1 fixture.
    //   2. Encrypting it via the walker.
    //   3. Asserting that every prop_fields() entry with is_secret =
    //      true whose dotted path is present in the generated config
    //      carries `enc2:` ciphertext at that path (or is empty).
    //
    // Adding a new `#[secret]` field to the schema automatically
    // joins the allowlist — no SECRET_KEY_NAMES const to maintain —
    // and this test verifies the resulting output gets encrypted.

    let tmp = tempfile::tempdir().expect("tempdir");
    let raw = generate(
        CURRENT_SCHEMA_VERSION,
        &GenerateOptions {
            encrypt_secrets: true,
            secret_store_dir: Some(tmp.path()),
        },
    )
    .expect("encrypted generate succeeds");
    let encrypted: toml::Value = toml::from_str(&raw).expect("encrypted output parses");
    let plaintext_cfg = migrate_to_current(V1_FIXTURE).expect("V1 fixture migrates");

    let mut missed = Vec::new();
    for field in plaintext_cfg
        .prop_fields()
        .into_iter()
        .filter(|f| f.is_secret)
    {
        let snake_path: String = field
            .name
            .split('.')
            .map(|seg| seg.replace('-', "_"))
            .collect::<Vec<_>>()
            .join(".");
        let leaf = lookup_dotted(&encrypted, &snake_path);
        match leaf {
            Some(toml::Value::String(s))
                if !s.is_empty() && !s.starts_with("enc2:") && !s.starts_with("enc:") =>
            {
                missed.push(format!("{snake_path} = {s:?}"));
            }
            Some(toml::Value::Array(items)) => {
                for (i, item) in items.iter().enumerate() {
                    if let toml::Value::String(s) = item
                        && !s.is_empty()
                        && !s.starts_with("enc2:")
                        && !s.starts_with("enc:")
                    {
                        missed.push(format!("{snake_path}[{i}] = {s:?}"));
                    }
                }
            }
            _ => {}
        }
    }
    assert!(
        missed.is_empty(),
        "schema-derived encrypt walker missed these #[secret] paths in \
         the generated config — the field exists in prop_fields() but its \
         string leaf survived as plaintext:\n\n{}",
        missed.join("\n")
    );
}

#[test]
fn encryption_covers_compound_map_secret_field() {
    // Map-shaped `#[secret]` fields (e.g. `mcp.servers[*].headers:
    // HashMap<String, String>`) don't surface through `prop_fields()`
    // — the derive intentionally skips non-Vec compound types. The
    // raw-TOML encrypt walker must therefore source its allowlist
    // from `secret_field_terminals()` (compile-time enumeration of
    // every `#[secret]` field at every depth), so map-shaped values
    // get the same encrypt-on-save coverage as scalar ones.
    //
    // This regression encodes that: a TOML config containing an MCP
    // headers table with bearer credentials must have every value
    // encrypted by the raw walker, while keys stay plaintext and
    // sibling non-secret strings (`url`, `name`) stay plaintext too.

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SecretStore::new(tmp.path(), true);

    let raw_toml = r#"
schema_version = 3

[[mcp.servers]]
name = "primary"
transport = "sse"
url = "https://mcp.example.invalid/sse"
command = ""

[mcp.servers.headers]
Authorization = "Bearer mcp-cred"
X-Tenant = "tenant-42"
"#;

    let mut value: toml::Value = toml::from_str(raw_toml).expect("toml parses");
    encrypt_secret_strings(&mut value, &store).expect("encrypt walker succeeds");

    let server = value
        .get("mcp")
        .and_then(|v| v.get("servers"))
        .and_then(toml::Value::as_array)
        .and_then(|arr| arr.first())
        .expect("mcp.servers[0] table");
    let headers = server
        .get("headers")
        .and_then(toml::Value::as_table)
        .expect("mcp.servers[0].headers table");

    for (key, val) in headers {
        let s = val
            .as_str()
            .unwrap_or_else(|| panic!("headers.{key} is not a string"));
        assert!(
            s.starts_with("enc2:"),
            "mcp.servers[0].headers.{key} must be enc2-prefixed; got: {s}"
        );
    }
    let auth = headers
        .get("Authorization")
        .and_then(toml::Value::as_str)
        .expect("Authorization value");
    let tenant = headers
        .get("X-Tenant")
        .and_then(toml::Value::as_str)
        .expect("X-Tenant value");
    assert_eq!(
        store.decrypt(auth).expect("decrypt auth"),
        "Bearer mcp-cred",
    );
    assert_eq!(store.decrypt(tenant).expect("decrypt tenant"), "tenant-42",);

    // Sibling non-secret strings remain plaintext — the walker only
    // descends through allowlisted keys, not every string in the tree.
    assert_eq!(
        server.get("url").and_then(toml::Value::as_str),
        Some("https://mcp.example.invalid/sse"),
    );
    assert_eq!(
        server.get("name").and_then(toml::Value::as_str),
        Some("primary"),
    );
}

#[test]
fn identity_lifts_into_agents_default_during_v2_to_v3() {
    // V2 had a top-level [identity] block. V3 demoted identity to
    // per-agent (`[agents.<alias>.identity]`); the V2->V3 typed
    // migration must lift the top-level block into the synthesized
    // default agent and remove the top-level key so the V3
    // deserializer doesn't see an unknown field.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[identity]
format = "aieos"
aieos_inline = "{\"placeholder\":true}"

[providers.models.openrouter]
model = "anthropic/claude-sonnet-4-5"
api_key = "sk-test"
"#,
    );

    assert!(
        v3.get("identity").is_none(),
        "top-level [identity] must not survive V2->V3 (V3 removed the slot); got: {v3:#?}"
    );

    let default_identity = lookup_dotted(&v3, "agents.default.identity")
        .expect("[agents.default.identity] must be lifted from V2 top-level [identity]");
    assert_eq!(
        default_identity.get("format").and_then(toml::Value::as_str),
        Some("aieos"),
        "lifted identity must preserve V2 format value"
    );
    assert_eq!(
        default_identity
            .get("aieos_inline")
            .and_then(toml::Value::as_str),
        Some("{\"placeholder\":true}"),
        "lifted identity must preserve V2 aieos_inline value"
    );
}

#[test]
fn identity_lift_does_not_clobber_operator_per_agent_block() {
    // If the operator already wrote a per-agent identity block in
    // their V2 input (forward-looking), the V2->V3 lift must not
    // overwrite it. Top-level [identity] is still removed (V3 has no
    // slot for it) but each per-agent block keeps its operator-set
    // value.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[identity]
format = "openclaw"

[providers.models.openrouter]
model = "anthropic/claude-sonnet-4-5"
api_key = "sk-test"

[agents.scout]
model_provider = "openrouter.openrouter"
risk_profile = "default"
runtime_profile = "default"

[agents.scout.identity]
format = "aieos"
"#,
    );

    let scout_identity = lookup_dotted(&v3, "agents.scout.identity")
        .expect("operator's per-agent identity must survive the V2->V3 fold");
    assert_eq!(
        scout_identity.get("format").and_then(toml::Value::as_str),
        Some("aieos"),
        "operator-set per-agent identity must NOT be clobbered by the top-level lift"
    );
    assert!(
        v3.get("identity").is_none(),
        "top-level [identity] must still be removed even when no agent needed the lift"
    );
}

/// Look up a dotted snake_case path inside a TOML value.
fn lookup_dotted<'a>(value: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    let mut cur = value;
    for segment in path.split('.') {
        let table = cur.as_table()?;
        cur = table.get(segment)?;
    }
    Some(cur)
}

#[test]
fn get_prop_resolves_model_field_for_typed_provider_alias() {
    // Reproduce: the dashboard's model-row click handler calls
    // getProp(`providers.models.<type>.<alias>.model`). If that path
    // doesn't resolve (or returns the wrong shape), the model→type
    // map stays empty and the click can't route to the provider's
    // Costs tab. Pinned with the user's exact config shape.
    use zeroclaw_config::schema::Config;
    let raw = r#"
schema_version = 3

[providers.models.anthropic.glados]
model = "claude-opus-4-7"
max_tokens = 25000

[providers.models.anthropic.clamps]
model = "claude-sonnet-4-6"
"#;
    let cfg: Config = toml::from_str(raw).expect("parse");
    let glados_model = cfg
        .get_prop("providers.models.anthropic.glados.model")
        .expect("get_prop must resolve for typed provider alias .model field");
    let clamps_model = cfg
        .get_prop("providers.models.anthropic.clamps.model")
        .expect("get_prop must resolve for the other alias too");
    eprintln!("glados.model = {glados_model:?}");
    eprintln!("clamps.model = {clamps_model:?}");
    assert_eq!(glados_model, "claude-opus-4-7");
    assert_eq!(clamps_model, "claude-sonnet-4-6");
}

#[test]
fn prop_fields_includes_providers_models_alias_model_path() {
    // The gateway's /api/config/prop handler does lookup_prop_field(&config, &path).
    // lookup_prop_field walks Config::prop_fields() and finds a matching entry.
    // If the model path isn't in prop_fields(), the gateway returns 404 and
    // the frontend's resolveModelToProviderType walk silently drops the alias.
    use zeroclaw_config::schema::Config;
    let raw = r#"
schema_version = 3

[providers.models.anthropic.glados]
model = "claude-opus-4-7"
"#;
    let cfg: Config = toml::from_str(raw).expect("parse");
    let want = "providers.models.anthropic.glados.model";
    let all = cfg.prop_fields();
    let found = all.iter().any(|f| f.name == want);
    if !found {
        let candidates: Vec<String> = all
            .iter()
            .filter(|f| f.name.contains("anthropic.glados"))
            .map(|f| f.name.clone())
            .collect();
        panic!("prop_fields() missing `{want}` — candidates: {candidates:?}",);
    }
}

#[test]
fn typed_family_root_is_not_a_map_keyed_section() {
    // Regression: ModelProviders is a typed struct (anthropic, openai, …
    // HashMap fields), not a single HashMap, so the typed-family root
    // doesn't resolve as a map-keyed section. Any frontend code that
    // walks providers.<category> must route through map_key_sections /
    // GET /api/config/templates instead.
    use zeroclaw_config::schema::Config;
    let raw = r#"
schema_version = 3
[providers.models.anthropic.glados]
model = "claude-opus-4-7"
"#;
    let cfg: Config = toml::from_str(raw).expect("parse");
    assert!(
        cfg.get_map_keys("providers.models").is_none(),
        "typed-family root must NOT resolve as a single map — typed wrapper has per-type HashMap fields",
    );
    assert_eq!(
        cfg.get_map_keys("providers.models.anthropic"),
        Some(vec!["glados".to_string()]),
        "the per-type slot IS a map keyed by alias",
    );
}

#[test]
fn map_key_sections_exposes_typed_family_slots() {
    // Regression: the dashboard's walkConfiguredModelBindings now reads
    // the slot list from /api/config/templates (backed by
    // Config::map_key_sections()). Verify every providers.<category>
    // typed-family root has its per-type slots registered there.
    use zeroclaw_config::schema::Config;
    let paths: std::collections::HashSet<&'static str> = Config::map_key_sections()
        .iter()
        .filter(|s| matches!(s.kind, zeroclaw_config::traits::MapKeyKind::Map))
        .map(|s| s.path)
        .collect();
    for required in [
        "providers.models.anthropic",
        "providers.models.openai",
        "providers.tts.openai",
        "providers.transcription.openai",
    ] {
        assert!(
            paths.contains(required),
            "map_key_sections() must expose `{required}` — frontend depends on this via /api/config/templates",
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Runtime-acceptance gate: every alias reference on every agent
// in a migrated V2 config resolves to a real config entry. Closes
// the migration-plan item "post-migration runtime accepts the
// migrated config, loads agents, and resolves all alias references"
// at the config layer (no live provider / channel / memory I/O).
// ─────────────────────────────────────────────────────────────

#[test]
fn migrated_v2_agent_alias_references_all_resolve() {
    // Two-agent V2 install with explicit per-agent brain + risk override.
    // Exercises the synthesized model-provider alias path, the
    // synthesized risk/runtime profile defaults, and the channels
    // back-reference rewrite.
    let v3 = migrate_v2(
        r#"
schema_version = 2

[autonomy]
level = "supervised"

[channels.telegram]
enabled = true
bot_token = "tg-tok"
agent = "scout"

[agents.scout]
provider = "anthropic"
model = "claude-sonnet-4-5"
api_key = "sk-ant-scout"

[agents.lead]
provider = "openai"
model = "gpt-4o"
api_key = "sk-openai-lead"
"#,
    );

    let serialized = toml::to_string(&v3).expect("V3 output serializes");
    let cfg: zeroclaw_config::schema::Config =
        toml::from_str(&serialized).expect("V3 output parses as Config");

    assert!(
        !cfg.agents.is_empty(),
        "V2 fixture must produce at least one agent"
    );

    for (alias, agent) in &cfg.agents {
        assert!(
            cfg.risk_profile_for_agent(alias).is_some(),
            "agent `{alias}` references risk_profile `{}` which does not resolve",
            agent.risk_profile
        );
        assert!(
            cfg.runtime_profile_for_agent(alias).is_some(),
            "agent `{alias}` references runtime_profile `{}` which does not resolve",
            agent.runtime_profile
        );
        assert!(
            cfg.model_provider_for_agent(alias).is_some(),
            "agent `{alias}` references model_provider `{}` which does not resolve",
            agent.model_provider
        );
    }
}
