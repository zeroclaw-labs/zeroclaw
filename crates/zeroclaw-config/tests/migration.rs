//! End-to-end migration tests for the V1 → V2 → V3 chain.

use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::migration::{
    CURRENT_SCHEMA_VERSION, GenerateOptions, MigrateReport, detect_version, encrypt_secret_strings,
    ensure_disk_at_current_version, generate, migrate_file, migrate_file_in_place,
    migrate_to_current,
};
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::v2::V2Config;
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_config::validation_warnings::ValidationWarning;

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
    let cfg = migrate_to_current(
        r#"
schema_version = 2

[channels.webhook]
port = 8080
"#,
    )
    .expect("V2 webhook block without enabled migrates");
    let webhook = cfg
        .channels
        .webhook
        .get("default")
        .expect("webhook.default survives into V3");
    assert!(
        !webhook.enabled,
        "V2 missing `enabled` deserializes to V3 enabled = false (matches V2 default)"
    );
}

#[test]
fn t7_enabled_field_preserved_on_surviving_instance() {
    // V3 keeps the `enabled` field on every channel config. Telegram in
    // the V1 fixture has enabled = true; migration ports the value
    // through verbatim and the typed config exposes it.
    let cfg = v3_config();
    let telegram = cfg
        .channels
        .telegram
        .get("default")
        .expect("channels.telegram.default in migrated config");
    assert!(
        telegram.enabled,
        "V2 enabled = true must port through to V3 channels.telegram.default.enabled"
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
fn t9_memory_qdrant_promoted_to_retired_storage_section() {
    // The qdrant backend was removed. A V2 [memory.qdrant] block still folds
    // into [storage.qdrant.default] (a retired surface that loads with a
    // warning), and the config keeps loading; selecting the backend then
    // fails explicitly at memory construction.
    let v3 = migrate_v2(
        r#"
[memory.qdrant]
url = "http://qdrant.example:6333"
collection = "fold_test_memories"
api_key = "qd-key"
"#,
    );
    let qdrant = v3
        .get("storage")
        .and_then(toml::Value::as_table)
        .and_then(|s| s.get("qdrant"))
        .and_then(toml::Value::as_table)
        .and_then(|q| q.get("default"))
        .and_then(toml::Value::as_table)
        .expect("[memory.qdrant] promoted to [storage.qdrant.default]");
    assert_eq!(
        qdrant.get("url").and_then(toml::Value::as_str),
        Some("http://qdrant.example:6333")
    );
    let raw = toml::to_string(&v3).unwrap();
    let warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
    assert!(
        warnings
            .iter()
            .any(|w| w.path == "storage.qdrant" && w.code == "memory_backend_removed"),
        "{warnings:?}"
    );
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
fn t9_memory_sqlite_open_timeout_loads_retired_config() {
    let raw = r#"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "sqlite"
auto_save = true
sqlite_open_timeout_secs = 60
"#;
    let cfg = migrate_to_current(raw).expect("V1 memory.sqlite migrates");
    assert_eq!(cfg.memory.backend, "sqlite");
}

#[test]
fn t10_storage_provider_postgres_still_loads() {
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
    // The postgres backend was removed: the V1 section still migrates and
    // the config still loads (the promoted [storage.postgres] table is a
    // retired surface reported by the tombstone warning).
    migrate_to_current(raw).expect("V1 storage.provider still migrates");
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
    let runtime = cfg
        .runtime_profiles
        .get("agent_complex_agent")
        .expect("synthesized runtime_profiles.agent_complex_agent");
    assert_eq!(
        runtime.max_tool_iterations, 25,
        "V2 max_iterations=25 must land on the synthesized runtime profile's max_tool_iterations"
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
    assert_eq!(
        risk.allowed_tools,
        Some(vec!["shell".to_string(), "memory".to_string()])
    );
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
fn t14c_max_depth_key_is_stripped_not_forwarded() {
    // The fixture's complex_agent carries V2 `max_depth = 4` alongside
    // `max_iterations = 25`. The retired spawn-depth key has no V3 field
    // (spawn wall): migration must NOT forward it, and the agent still
    // points at the profile synthesized from the surviving keys (that
    // synthesis is caused by max_iterations/agentic, per t14a/t14b —
    // this test pins the STRIP half).
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.runtime_profile, "agent_complex_agent",
        "surviving V2 runtime keys must still point the agent at its synthesized profile"
    );
    let profile = cfg
        .runtime_profiles
        .get("agent_complex_agent")
        .expect("synthesized runtime_profiles.agent_complex_agent");
    assert_eq!(profile.max_tool_iterations, 25);
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
}

#[test]
fn v2_whatsapp_does_not_activate_or_synthesize_peer_groups() {
    let raw = r#"schema_version = 2
[channels.whatsapp]
enabled = true
session_path = "/tmp/retired-whatsapp.db"
access_token = "retired-token"
app_secret = "retired-secret"
allowed_numbers = ["+15555550102"]
allowed_groups = ["group-id"]
dm_policy = "allowlist"
group_policy = "allowlist"

[channels.webhook]
enabled = true
port = 8080
"#;
    let config = migrate_to_current(raw).expect("retired WhatsApp config must load");
    assert_eq!(config.channels.webhook["default"].port, 8080);
    assert!(
        config.peer_groups.is_empty(),
        "retired allowlists must not become live peer groups"
    );
    assert!(!config.channels.is_known_channel("whatsapp"));
    assert!(
        !toml::to_string(&config)
            .unwrap()
            .contains("[channels.whatsapp")
    );
    let warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(raw);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].path, "channels.whatsapp");
    assert_eq!(warnings[0].code, "retired_channel_transport_removed");
}

#[test]
fn v2_retired_inbound_webhook_channels_are_dropped_not_ported() {
    // Linq, WATI, Nextcloud Talk and Gmail Push lost their gateway webhook
    // routes and were deleted. V2 → V3 must drop their tables (and must not
    // synthesize peer groups from their allowlists) while live siblings keep
    // migrating.
    let v3 = migrate_v2(
        r#"
[channels.linq]
enabled = true
api_token = "linq-tok"
from_phone = "+15555550100"
allowed_senders = ["+15551234567"]

[channels.wati]
enabled = true
api_token = "wati-tok"
allowed_numbers = ["+15551234567"]

[channels.nextcloud_talk]
enabled = true
base_url = "https://cloud.example.com"
app_token = "nc-tok"
allowed_users = ["alice"]

[channels.gmail_push]
enabled = true
topic = "projects/p/topics/t"
allowed_senders = ["ops@example"]

[channels.webhook]
enabled = true
port = 8080
"#,
    );

    let channels = v3
        .get("channels")
        .and_then(toml::Value::as_table)
        .expect("channels exists");
    let peer_groups = v3.get("peer_groups").and_then(toml::Value::as_table);
    for retired in ["linq", "wati", "nextcloud_talk", "gmail_push"] {
        assert!(
            !channels.contains_key(retired),
            "channels.{retired} must be dropped during V2 → V3 migration"
        );
        assert!(
            peer_groups.is_none_or(|pg| !pg.contains_key(&format!("{retired}_default"))),
            "no peer group may be synthesized for retired channel {retired}"
        );
    }
    assert!(
        channels
            .get("webhook")
            .and_then(toml::Value::as_table)
            .is_some_and(|t| t.contains_key("default")),
        "live sibling channel must still migrate"
    );
}

#[test]
fn v3_retired_inbound_webhook_channel_loads_with_tombstone_warning() {
    // A V3 config still carrying a retired channel section, an agent binding
    // to it and a peer group on it must keep loading: the section is ignored
    // and reported as a structured tombstone warning, not a hard error.
    let mut value: toml::Value = toml::from_str(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 parses");
    let root = value.as_table_mut().unwrap();
    let linq: toml::Value = toml::from_str(
        r#"
[default]
enabled = true
api_token = "linq-tok"
from_phone = "+15555550100"
"#,
    )
    .unwrap();
    root.get_mut("channels")
        .and_then(toml::Value::as_table_mut)
        .expect("generated V3 has channels")
        .insert("linq".into(), linq);
    let (agent_alias, agent) = root
        .get_mut("agents")
        .and_then(toml::Value::as_table_mut)
        .and_then(|agents| agents.iter_mut().next())
        .expect("generated V3 has an agent");
    let agent_alias = agent_alias.clone();
    agent
        .as_table_mut()
        .unwrap()
        .entry("channels")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .unwrap()
        .push("linq.default".into());
    let group: toml::Value = toml::from_str(&format!(
        "channel = \"linq\"\nagents = [\"{agent_alias}\"]\nexternal_peers = [\"+15551234567\"]\n"
    ))
    .unwrap();
    root.entry("peer_groups")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap()
        .insert("linq_default".into(), group);
    let raw = toml::to_string(&value).unwrap();

    let cfg = migrate_to_current(&raw).expect("retired channel section must not fail load");
    cfg.validate()
        .expect("leftover bindings to a retired channel type must not fail validation");

    let warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, "inbound_webhook_channel_removed");
    assert_eq!(warnings[0].path, "channels.linq");
}

#[test]
fn v3_retired_wss_section_loads_with_tombstone_warning() {
    // The daemon's JSON-RPC socket and WSS listener were retired. A V3
    // config still carrying `[wss]` must keep loading: the section is
    // ignored and reported as a structured tombstone warning.
    let mut value: toml::Value = toml::from_str(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 parses");
    let wss: toml::Value = toml::from_str(
        r#"
enabled = true
bind = "0.0.0.0"
port = 9781
cert_path = "/etc/zeroclaw/cert.pem"
key_path = "/etc/zeroclaw/key.pem"
"#,
    )
    .unwrap();
    value.as_table_mut().unwrap().insert("wss".into(), wss);
    let raw = toml::to_string(&value).unwrap();

    let cfg = migrate_to_current(&raw).expect("retired [wss] section must not fail load");
    cfg.validate()
        .expect("retired [wss] section must not fail validation");

    let warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, "wss_transport_removed");
    assert_eq!(warnings[0].path, "wss");
}

#[test]
fn v3_retired_dead_feature_sections_load_with_tombstone_warnings() {
    // The A2A server/agent cards, the ZeroCode todotracker and the
    // never-wired conversational_ai section lost their last consumers and
    // were deleted. Configs still carrying them must keep loading, with one
    // structured tombstone warning each.
    for (path, body, code) in [
        ("a2a", "enabled = true", "a2a_server_removed"),
        ("todotracker", "enabled = true", "todo_tracker_removed"),
        (
            "conversational_ai",
            "enabled = true",
            "conversational_ai_removed",
        ),
    ] {
        let (raw, warnings) = load_with_retired_section(path, body);
        let cfg = migrate_to_current(&raw)
            .unwrap_or_else(|e| panic!("[{path}] must not fail load: {e:#}"));
        cfg.validate()
            .unwrap_or_else(|e| panic!("[{path}] must not fail validation: {e:#}"));
        assert_eq!(warnings.len(), 1, "[{path}]: {warnings:?}");
        assert_eq!(warnings[0].code, code, "[{path}]");
        assert_eq!(warnings[0].path, path);
    }
}

/// Insert `section` (a TOML table body) at dotted `path` into a freshly
/// generated current-schema config, load it, and return the raw text plus
/// the tombstone warnings it produces.
fn load_with_retired_section(path: &str, body: &str) -> (String, Vec<ValidationWarning>) {
    let mut value: toml::Value = toml::from_str(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 parses");
    let mut table = value.as_table_mut().unwrap();
    let segments: Vec<&str> = path.split('.').collect();
    let (last, parents) = segments.split_last().unwrap();
    for segment in parents {
        table = table
            .entry(segment.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .unwrap();
    }
    table.insert((*last).to_string(), toml::from_str(body).unwrap());
    let raw = toml::to_string(&value).unwrap();

    let cfg = migrate_to_current(&raw)
        .unwrap_or_else(|e| panic!("retired [{path}] section must not fail load: {e}"));
    cfg.validate()
        .unwrap_or_else(|e| panic!("retired [{path}] section must not fail validation: {e}"));
    let mut warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
    warnings.extend(zeroclaw_config::validation_warnings::retired_field_tombstones(&raw));
    (raw, warnings)
}

#[test]
fn v3_retired_plugins_section_loads_with_tombstone_warning() {
    // The WASM plugin host was retired (extensions go through MCP). A V3
    // config still carrying `[plugins]` must keep loading with a warning.
    let (_, warnings) = load_with_retired_section(
        "plugins",
        r#"
enabled = true
plugins_dir = "~/.zeroclaw/plugins"
auto_discover = true
max_plugins = 5

[security]
signature_mode = "strict"
trusted_publisher_keys = ["00"]

[limits]
call_fuel = 1000

[[entries]]
name = "weather"
config = { api_key = "k" }
"#,
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, "wasm_plugins_removed");
    assert_eq!(warnings[0].path, "plugins");
}

#[test]
fn v3_retired_hardware_sections_load_with_tombstone_warnings() {
    // In-process hardware left the body (ADR-017: devices join as Nodes).
    // `[hardware]` and `[peripherals]` must keep loading with a warning.
    for (path, body) in [
        (
            "hardware",
            "enabled = true\ntransport = \"serial\"\nserial_port = \"/dev/ttyACM0\"\nbaud_rate = 115200\n",
        ),
        (
            "peripherals",
            "enabled = true\ndatasheet_dir = \"docs/datasheets\"\n\n[[boards]]\nboard = \"nucleo-f401re\"\ntransport = \"serial\"\npath = \"/dev/ttyACM0\"\n",
        ),
    ] {
        let (_, warnings) = load_with_retired_section(path, body);
        assert_eq!(warnings.len(), 1, "{path}: {warnings:?}");
        assert_eq!(warnings[0].code, "hardware_moved_out_of_core");
        assert_eq!(warnings[0].path, path);
    }
}

#[test]
fn v3_retired_saas_sections_load_with_tombstone_warnings() {
    // The vendor SaaS tool families were retired. Their sections must keep
    // loading with a warning, including nested tables and secrets.
    for (path, body) in [
        (
            "jira",
            "enabled = true\nbase_url = \"https://example.atlassian.net\"\napi_token = \"tok\"\nallowed_actions = [\"get_ticket\"]\n",
        ),
        (
            "linkedin",
            "enabled = true\napi_version = \"202602\"\n\n[content]\ntopics = [\"rust\"]\n\n[image]\nenabled = true\n",
        ),
        (
            "microsoft365",
            "enabled = true\ntenant_id = \"t\"\nclient_id = \"c\"\nauth_flow = \"device_code\"\n",
        ),
        (
            "google_workspace",
            "enabled = true\nallowed_services = [\"gmail\"]\n\n[[allowed_operations]]\nservice = \"gmail\"\nresource = \"users\"\nmethods = [\"list\"]\n",
        ),
        ("cloud_ops", "enabled = true\ndefault_cloud = \"aws\"\n"),
        (
            "project_intel",
            "enabled = true\ndefault_language = \"en\"\n",
        ),
    ] {
        let (_, warnings) = load_with_retired_section(path, body);
        assert_eq!(warnings.len(), 1, "{path}: {warnings:?}");
        assert_eq!(warnings[0].code, "saas_integration_removed");
        assert_eq!(warnings[0].path, path);
    }
}

#[test]
fn v3_retired_otel_backend_and_fields_load_with_tombstone_warnings() {
    // The OpenTelemetry exporter was removed. `backend = "otel"` and the
    // `otel_*` keys must keep loading: the backend falls back to `none` and
    // every retired key is reported, never silently dropped.
    let mut value: toml::Value = toml::from_str(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 parses");
    let observability = value
        .as_table_mut()
        .unwrap()
        .entry("observability")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    let retired: toml::Table = toml::from_str(
        r#"
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
otel_genai_content = "redacted"
otel_genai_content_max_chars = 500
otel_tool_io = "full"
otel_tool_io_max_chars = 500

[otel_headers]
Authorization = "Bearer x"
"#,
    )
    .unwrap();
    observability.extend(retired);
    let raw = toml::to_string(&value).unwrap();

    let cfg = migrate_to_current(&raw).expect("retired otel config must not fail load");
    cfg.validate()
        .expect("retired otel config must not fail validation");
    assert_eq!(
        cfg.observability.backend,
        zeroclaw_config::schema::ObservabilityBackend::None
    );

    let warnings = zeroclaw_config::validation_warnings::retired_field_tombstones(&raw);
    assert_eq!(warnings.len(), 8, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .all(|w| w.code == "otel_observability_removed"),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.path == "observability.backend"),
        "{warnings:?}"
    );
}

#[test]
fn v3_retired_memory_and_browser_backends_load_with_tombstone_warnings() {
    // postgres/qdrant/sqlite/lucid memory (plus snapshot_* / sqlite timeout)
    // and the rust_native browser backend were removed. A config that still
    // selects them must keep loading and report every retired selector;
    // selecting them then fails explicitly at construction (memory factory /
    // browser tool), never silently.
    let mut value: toml::Value = toml::from_str(
        &generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
            .expect("generate current succeeds"),
    )
    .expect("generated V3 parses");
    let root = value.as_table_mut().unwrap();
    let retired: toml::Table = toml::from_str(
        r#"
[memory]
backend = "postgres.work"
sqlite_open_timeout_secs = 30
snapshot_enabled = true
snapshot_on_hygiene = true
auto_hydrate = false

[storage.postgres.work]
db_url = "postgres://user:pw@host/db"
vector_enabled = true

[storage.qdrant.default]
url = "http://localhost:6333"

[storage.sqlite.default]
open_timeout_secs = 30

[storage.lucid.default]
binary_path = "/opt/lucid/bin/lucid"

[browser]
backend = "rust_native"
native_headless = true
native_webdriver_url = "http://127.0.0.1:9515"
native_chrome_path = "/usr/bin/chromium"
"#,
    )
    .unwrap();
    for (key, table) in retired {
        let slot = root
            .entry(key)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .unwrap();
        for (k, v) in table.as_table().unwrap() {
            match (slot.get_mut(k).and_then(toml::Value::as_table_mut), v) {
                (Some(existing), toml::Value::Table(inner)) => existing.extend(inner.clone()),
                _ => {
                    slot.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let raw = toml::to_string(&value).unwrap();

    let cfg = migrate_to_current(&raw).expect("retired backends must not fail load");
    assert_eq!(cfg.memory.backend, "postgres.work");

    let mut warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
    warnings.extend(zeroclaw_config::validation_warnings::retired_field_tombstones(&raw));
    let mut hits: Vec<(&str, &str)> = warnings
        .iter()
        .map(|w| (w.path.as_str(), w.code.as_str()))
        .collect();
    hits.sort_unstable();
    assert_eq!(
        hits,
        vec![
            ("browser.backend", "browser_native_backend_removed"),
            (
                "browser.native_chrome_path",
                "browser_native_backend_removed"
            ),
            ("browser.native_headless", "browser_native_backend_removed"),
            (
                "browser.native_webdriver_url",
                "browser_native_backend_removed"
            ),
            ("memory.auto_hydrate", "memory_snapshot_removed"),
            ("memory.backend", "memory_backend_removed"),
            ("memory.snapshot_enabled", "memory_snapshot_removed"),
            ("memory.snapshot_on_hygiene", "memory_snapshot_removed"),
            ("memory.sqlite_open_timeout_secs", "memory_backend_removed"),
            ("storage.lucid", "memory_backend_removed"),
            ("storage.postgres", "memory_backend_removed"),
            ("storage.qdrant", "memory_backend_removed"),
            ("storage.sqlite", "memory_backend_removed"),
        ],
        "{warnings:?}"
    );

    for legacy_backend in ["sqlite", "lucid"] {
        let mut backend_value = value.clone();
        backend_value["memory"]["backend"] = toml::Value::String(legacy_backend.to_string());
        let raw_legacy = toml::to_string(&backend_value).unwrap();
        let legacy_cfg = migrate_to_current(&raw_legacy).expect("legacy backend still loads");
        assert_eq!(legacy_cfg.memory.backend, legacy_backend);
        let field_warnings =
            zeroclaw_config::validation_warnings::retired_field_tombstones(&raw_legacy);
        assert!(
            field_warnings
                .iter()
                .any(|w| w.path == "memory.backend" && w.code == "memory_backend_removed"),
            "expected memory_backend_removed for {legacy_backend}, got {field_warnings:?}"
        );
    }
}

#[test]
fn v2_channels_voice_duplex_block_alias_wraps() {
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
    use std::collections::HashSet;
    use zeroclaw_config::schema::Config;
    use zeroclaw_config::schema::v2::V3_CHANNEL_TYPES;

    let listed: HashSet<&str> = V3_CHANNEL_TYPES.iter().copied().collect();

    // Channel types that are intentionally NOT in V3_CHANNEL_TYPES:
    // they're folded into another channel type by a dedicated walker
    // step that runs before the alias-wrap loop.
    let folded_into_sibling: HashSet<&str> = HashSet::new();

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

#[test]
fn generate_every_version_migrates_and_validates() {
    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(target, &GenerateOptions::default())
            .unwrap_or_else(|e| panic!("generate({target}) failed: {e:#}"));
        let cfg = migrate_to_current(&raw).unwrap_or_else(|e| {
            panic!("generate({target}) output failed to migrate to current schema: {e:#}")
        });
        cfg.validate()
            .unwrap_or_else(|e| panic!("generate({target}) output fails Config::validate: {e:#}"));
    }
}

#[test]
fn generate_never_emits_retired_config_surfaces() {
    // The seed fixture predates retirements and the migration lenses pass
    // unknown nested tables through, so the generator must strip retired
    // sections itself — a freshly generated config must not trigger the
    // running binary's own removal warning on first load. Asserted through
    // the real detector so every retired surface in the shared table is
    // covered without re-implementing the walk.
    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(target, &GenerateOptions::default())
            .unwrap_or_else(|e| panic!("generate({target}) failed: {e:#}"));
        // Pin WhatsApp independently of the tombstone registry: forgetting its
        // registration must still fail for all generator schema versions.
        let value: toml::Value = toml::from_str(&raw).unwrap();
        let root = if target == 1 {
            "channels_config"
        } else {
            "channels"
        };
        let channels = value[root].as_table().unwrap();
        assert!(!channels.contains_key("whatsapp"));
        for retained in ["telegram", "discord", "webhook", "email"] {
            assert!(
                channels.contains_key(retained),
                "generate({target}) must retain {retained}"
            );
        }
        assert!(
            value
                .get("transcription")
                .and_then(|transcription| transcription.get("transcribe_non_ptt_audio"))
                .is_none()
        );
        let mut warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(&raw);
        warnings.extend(zeroclaw_config::validation_warnings::retired_field_tombstones(&raw));
        assert!(
            warnings.is_empty(),
            "generate({target}) must not emit retired config surfaces or fields: {warnings:?}"
        );
    }
}

#[test]
fn generate_never_emits_retired_batch_b_channel_settings() {
    // Literal expectations also catch a missing tombstone registration; the
    // general detector-derived test cannot discriminate that omission.
    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(target, &GenerateOptions::default()).unwrap();
        let value: toml::Value = toml::from_str(&raw).unwrap();
        let root = if target == 1 {
            "channels_config"
        } else {
            "channels"
        };
        let channels = value[root].as_table().unwrap();
        for kind in [
            "line", "dingtalk", "wecom", "wecom_ws", "wechat", "qq", "twitter", "mochat",
        ] {
            assert!(
                !channels.contains_key(kind),
                "generate({target}) must omit {kind}"
            );
        }
        assert!(channels.contains_key("discord"));
        assert!(channels.contains_key("email"));
    }
}

#[test]
fn generate_never_emits_retired_batch_c_channel_settings() {
    for target in 1..=CURRENT_SCHEMA_VERSION {
        let raw = generate(target, &GenerateOptions::default()).unwrap();
        let value: toml::Value = toml::from_str(&raw).unwrap();
        let root = if target == 1 {
            "channels_config"
        } else {
            "channels"
        };
        let channels = value[root].as_table().unwrap();
        for kind in [
            "nostr",
            "clawdtalk",
            "reddit",
            "bluesky",
            "git",
            "voice_call",
            "voice_wake",
            "amqp",
        ] {
            assert!(
                !channels.contains_key(kind),
                "generate({target}) must omit {kind}"
            );
        }
        assert!(channels.contains_key("discord"));
        assert!(channels.contains_key("email"));
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
fn generate_v3_preserves_retained_channels_and_retires_transports() {
    // Pin the fixture's retained aliases and meaningful fields, rather than a
    // count another alias could accidentally satisfy after losing a channel.
    let generated = generate(CURRENT_SCHEMA_VERSION, &GenerateOptions::default())
        .expect("generate current succeeds");
    let cfg = migrate_to_current(&generated).expect("generated V3 migrates");
    let channels = serde_json::to_value(&cfg.channels).expect("serialize channel configuration");
    let expected_fields = serde_json::json!({
        "telegram": {
            "enabled": true,
            "bot_token": "telegram-v1-bot-token",
            "stream_mode": "partial",
            "draft_update_interval_ms": 1000,
            "interrupt_on_new_message": false,
            "mention_only": false,
            "ack_reactions": false
        },
        "discord": {
            "enabled": true,
            "bot_token": "discord-v1-bot-token",
            "guild_ids": ["111122223333"],
            "channel_ids": ["555566667777"],
            "archive": true,
            "listen_to_bots": false,
            "mention_only": true,
            "stream_mode": "off",
            "draft_update_interval_ms": 1000,
            "multi_message_delay_ms": 500,
            "stall_timeout_secs": 0
        },
        "webhook": {
            "enabled": true,
            "port": 8080,
            "listen_path": "/webhook",
            "send_url": "https://hooks.example.com/inbound",
            "send_method": "POST"
        },
        "email": {
            "enabled": false,
            "imap_host": "imap.example.com",
            "smtp_host": "smtp.example.com",
            "username": "zeroclaw@example.com",
            "password": "email-password",
            "from_address": "zeroclaw@example.com"
        }
    });
    let expected_fields = expected_fields.as_object().unwrap();
    let expected_aliases: std::collections::BTreeSet<_> = expected_fields
        .keys()
        .map(|kind| format!("{kind}.default"))
        .collect();
    let mut actual_aliases = std::collections::BTreeSet::new();
    for (kind, value) in channels.as_object().unwrap() {
        if let Some(aliases) = value.as_object() {
            for alias in aliases.keys() {
                actual_aliases.insert(format!("{kind}.{alias}"));
            }
        }
    }
    assert_eq!(
        actual_aliases, expected_aliases,
        "retained channel aliases must migrate exactly"
    );
    for (kind, fields) in expected_fields {
        for (field, expected) in fields.as_object().unwrap() {
            assert_eq!(
                channels[kind]["default"][field], *expected,
                "channels.{kind}.default.{field} must preserve the fixture's configuration"
            );
        }
    }

    // These are every retained allowlist in the V1 fixture. Check the whole
    // peer set and its scope, so dropping or widening a restriction fails.
    // The fixture has simple_agent/complex_agent, but no agents.default.
    // rewrite_dangling_peer_group_agents assigns each synthesized group to
    // the first agent key, complex_agent, and binds its channel aliases there.
    let expected_peers = serde_json::json!({
        "telegram": {
            "agents": ["complex_agent"],
            "external_peers": ["@v1_admin", "12345678"]
        },
        "discord": {
            "agents": ["complex_agent"],
            "external_peers": ["222233334444"]
        }
    });
    let expected_peers = expected_peers.as_object().unwrap();
    let expected_groups: std::collections::BTreeSet<_> = expected_peers
        .keys()
        .map(|kind| format!("{kind}_default"))
        .collect();
    assert_eq!(
        cfg.peer_groups
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        expected_groups,
        "only retained allowlists may synthesize peer groups"
    );
    let expected_agent_channels: std::collections::BTreeSet<_> = expected_peers
        .keys()
        .map(|kind| format!("{kind}.default"))
        .collect();
    assert_eq!(
        cfg.agents["complex_agent"]
            .channels
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::BTreeSet<_>>(),
        expected_agent_channels,
        "the migrated owner must bind exactly the peer groups' retained aliases"
    );
    assert!(cfg.agents["simple_agent"].channels.is_empty());
    for (kind, expected) in expected_peers {
        let name = format!("{kind}_default");
        let group = serde_json::to_value(&cfg.peer_groups[&name]).unwrap();
        assert_eq!(
            group["channel"],
            serde_json::json!(kind),
            "{name} channel scope"
        );
        assert_eq!(group["agents"], expected["agents"], "{name} agent scope");
        assert_eq!(
            group["external_peers"], expected["external_peers"],
            "{name} complete allowlist"
        );
        assert_eq!(group["ignore"], serde_json::json!([]), "{name} blocklist");
        assert_eq!(
            group["admin_for_agent_scope"], false,
            "{name} must not grant admin"
        );
    }

    for kind in [
        "line",
        "dingtalk",
        "wecom",
        "wecom_ws",
        "wechat",
        "qq",
        "twitter",
        "mochat",
        "nostr",
        "clawdtalk",
        "reddit",
        "bluesky",
        "git",
        "voice_call",
        "voice_wake",
        "amqp",
        "whatsapp",
    ] {
        assert!(
            channels.get(kind).is_none(),
            "retired {kind} must have no alias"
        );
        assert!(!cfg.peer_groups.contains_key(&format!("{kind}_default")));
    }

    // The original fixture still exercises real legacy WhatsApp auth and
    // allowlist settings. Retirement must be explicit and must not advertise
    // a replacement or carry those restrictions onto another transport.
    assert!(channels.get("whatsapp").is_none());
    assert!(!cfg.peer_groups.contains_key("whatsapp_default"));
    let warnings = zeroclaw_config::validation_warnings::retired_section_tombstones(V1_FIXTURE);
    let whatsapp_warnings: Vec<_> = warnings
        .iter()
        .filter(|warning| warning.path == "channels_config.whatsapp")
        .collect();
    assert_eq!(
        whatsapp_warnings.len(),
        1,
        "legacy WhatsApp must warn explicitly"
    );
    let warning = whatsapp_warnings[0];
    assert_eq!(warning.code, "retired_channel_transport_removed");
    assert!(warning.message.contains("no runtime consumer"));
    assert!(warning.message.contains("Remove the section."));
    let fixture: toml::Value = toml::from_str(V1_FIXTURE).unwrap();
    for field in ["access_token", "app_secret", "verify_token"] {
        let secret = fixture["channels_config"]["whatsapp"][field]
            .as_str()
            .unwrap();
        assert!(
            !warning.message.contains(secret),
            "retirement must not expose {field}"
        );
    }
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
fn encryption_preserves_onepassword_secret_references() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SecretStore::new(tmp.path(), true);

    let raw_toml = r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-5"
api_key = "op://zeroclaw/provider/openai-api-key"
"#;

    let mut value: toml::Value = toml::from_str(raw_toml).expect("toml parses");
    encrypt_secret_strings(&mut value, &store).expect("encrypt walker succeeds");

    let api_key = lookup_dotted(&value, "providers.models.openai.default.api_key")
        .and_then(toml::Value::as_str);

    assert_eq!(api_key, Some("op://zeroclaw/provider/openai-api-key"));
}

#[test]
fn encryption_preserves_tachi_vault_secret_references() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SecretStore::new(tmp.path(), true);

    let raw_toml = r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#;

    let mut value: toml::Value = toml::from_str(raw_toml).expect("toml parses");
    encrypt_secret_strings(&mut value, &store).expect("encrypt walker succeeds");

    let api_key = lookup_dotted(&value, "providers.models.openai.default.api_key")
        .and_then(toml::Value::as_str);

    assert_eq!(api_key, Some("vault:OPENAI_OFFICIAL_API_KEY"));
}

#[test]
fn identity_lifts_into_agents_default_during_v2_to_v3() {
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
