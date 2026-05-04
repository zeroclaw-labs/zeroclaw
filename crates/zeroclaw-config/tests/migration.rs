//! End-to-end migration tests for the V1 → V2 → V3 chain.
//!
//! Sole input: `tests/fixtures/v1.toml`, embedded via `include_str!` so it
//! lives only in the test binary. No fixture files for V2 or V3 — V2/V3
//! shape is asserted via typed deserialization (`Config`) and `toml::Value`
//! navigation on the migration output.
//!
//! One test per transform listed in the plan's Step 0 ground truth. Each
//! test asserts the destination value present in V3 output; if the migration
//! step that performs the transform is broken, the test fails.

use zeroclaw_config::migration::{
    CURRENT_SCHEMA_VERSION, MigrateReport, detect_version, ensure_disk_at_current_version,
    migrate_file, migrate_file_in_place, migrate_to_current,
};
use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::v2::V2Config;

const V1_FIXTURE: &str = include_str!("fixtures/v1.toml");

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
fn migrate_v2(input: &str) -> toml::Value {
    let v2: V2Config = toml::from_str(input).expect("V2 input parses as V2Config");
    v2.migrate().expect("V2 → V3 migration succeeds")
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
// V1 globals → V2 [providers] → V3 providers.models.<type>.default
// ─────────────────────────────────────────────────────────────

#[test]
fn v1_default_provider_target_holds_globals() {
    let cfg = v3_config();
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
        "V1 api_url renamed to base_url on the per-provider entry"
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
    let anth = v3
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|p| p.get("models"))
        .and_then(toml::Value::as_table)
        .and_then(|m| m.get("anthropic"))
        .and_then(toml::Value::as_table)
        .and_then(|a| a.get("default"))
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
    let providers_models = v3
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|p| p.get("models"))
        .and_then(toml::Value::as_table)
        .expect("providers.models present after V2→V3");
    let cc = providers_models
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
        !providers_models.contains_key("claude-code"),
        "standalone claude-code provider must not appear in V3"
    );
}

#[test]
fn v1_model_routes_preserved_at_providers_level() {
    let cfg = v3_config();
    assert!(
        !cfg.providers.model_routes.is_empty(),
        "model_routes survive into providers.model_routes"
    );
}

// ─────────────────────────────────────────────────────────────
// T1, T2 — V1→V2 channel singular→plural folds
// ─────────────────────────────────────────────────────────────

#[test]
fn t1_matrix_room_id_folds_into_allowed_rooms() {
    let cfg = v3_config();
    let matrix = cfg
        .channels
        .matrix
        .get("default")
        .expect("channels.matrix.default exists after enabled-keep");
    assert!(
        matrix
            .allowed_rooms
            .iter()
            .any(|r| r == "!important-room:matrix.org"),
        "V1 matrix.room_id was not folded into V3 channels.matrix.default.allowed_rooms[]; \
         got {:?}",
        matrix.allowed_rooms
    );
}

#[test]
fn t2_slack_channel_id_folds_into_channel_ids() {
    let cfg = v3_config();
    let slack = cfg
        .channels
        .slack
        .get("default")
        .expect("channels.slack.default exists");
    assert!(
        slack.channel_ids.iter().any(|c| c == "C01ABCD0001"),
        "V1 slack.channel_id was not folded into V3 channels.slack.default.channel_ids[]; \
         got {:?}",
        slack.channel_ids
    );
}

// ─────────────────────────────────────────────────────────────
// T3-T6 — V2→V3 channel singular→plural folds
// ─────────────────────────────────────────────────────────────

#[test]
fn t3_discord_guild_id_folds_into_guild_ids() {
    let cfg = v3_config();
    let discord = cfg
        .channels
        .discord
        .get("default")
        .expect("channels.discord.default exists");
    assert!(
        discord.guild_ids.iter().any(|g| g == "11111"),
        "V2 discord.guild_id was not folded into V3 guild_ids[]; got {:?}",
        discord.guild_ids
    );
}

#[test]
fn t4_mattermost_channel_id_folds_into_channel_ids() {
    let cfg = v3_config();
    let mm = cfg
        .channels
        .mattermost
        .get("default")
        .expect("channels.mattermost.default exists");
    assert!(
        mm.channel_ids.iter().any(|c| c == "mm-channel-001"),
        "V2 mattermost.channel_id was not folded into V3 channel_ids[]; got {:?}",
        mm.channel_ids
    );
}

#[test]
fn t5_reddit_subreddit_folds_into_subreddits() {
    let cfg = v3_config();
    let reddit = cfg
        .channels
        .reddit
        .get("default")
        .expect("channels.reddit.default exists");
    assert!(
        reddit.subreddits.iter().any(|s| s == "rust"),
        "V2 reddit.subreddit was not folded into V3 subreddits[]; got {:?}",
        reddit.subreddits
    );
}

#[test]
fn t6_signal_group_id_folds_into_group_ids() {
    let cfg = v3_config();
    let signal = cfg
        .channels
        .signal
        .get("default")
        .expect("channels.signal.default exists");
    assert!(
        signal.group_ids.iter().any(|g| g == "group-abc-001"),
        "V2 signal.group_id was not folded into V3 group_ids[]; got {:?}",
        signal.group_ids
    );
    // The fixture's signal.group_id is NOT "dm", so dm_only should remain false.
    assert!(
        !signal.dm_only,
        "non-\"dm\" group_id must not set dm_only=true"
    );
}

// ─────────────────────────────────────────────────────────────
// T7 — channel `enabled` semantics
// ─────────────────────────────────────────────────────────────

#[test]
fn t7_enabled_false_channel_dropped() {
    let cfg = v3_config();
    assert!(
        cfg.channels.webhook.is_empty(),
        "V2 webhook with enabled=false must be dropped from V3 channels.webhook \
         (V3 has no off-switch other than absence); got {:?}",
        cfg.channels.webhook.keys().collect::<Vec<_>>()
    );
}

#[test]
fn t7_enabled_unset_channel_dropped() {
    let cfg = v3_config();
    assert!(
        cfg.channels.imessage.is_empty(),
        "V2 imessage without explicit enabled must be dropped (defaulted to false); \
         got {:?}",
        cfg.channels.imessage.keys().collect::<Vec<_>>()
    );
}

#[test]
fn t7_enabled_field_stripped_from_surviving_instance() {
    // V3 channel configs have no `enabled` field; the migration must strip it
    // before alias-wrapping. We assert by checking the raw migrated TOML.
    let value = v3_value();
    let matrix_default = value
        .get("channels")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("matrix"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .expect("channels.matrix.default in migrated TOML");
    assert!(
        !matrix_default.contains_key("enabled"),
        "V2 enabled field must be stripped from surviving V3 channel instances"
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
    let value = v3_value();
    // [tts.openai] should be GONE from [tts] (moved to providers.tts.openai.default)
    let tts_table = value
        .get("tts")
        .and_then(toml::Value::as_table)
        .expect("[tts] retained for top-level scalars");
    assert!(
        !tts_table.contains_key("openai"),
        "V2 [tts.openai] sub-block must be moved out of [tts]"
    );

    // And it should appear at providers.tts.openai.default with the api_key.
    let api_key = value
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("tts"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("openai"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("api_key"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        api_key,
        Some("sk-tts-openai"),
        "V2 [tts.openai].api_key did not land at providers.tts.openai.default.api_key"
    );

    // ElevenLabs V2 `model_id` must be renamed to V3 `model`.
    let eleven_default = value
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("tts"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("elevenlabs"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
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
fn t8_tts_default_provider_rewritten_as_dotted_alias() {
    let value = v3_value();
    let dp = value
        .get("tts")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default_provider"))
        .and_then(toml::Value::as_str);
    assert_eq!(
        dp,
        Some("openai.default"),
        "V2 tts.default_provider=\"openai\" must be rewritten as dotted V3 alias \"openai.default\""
    );
}

// ─────────────────────────────────────────────────────────────
// T9 + T10 — storage subsystem promotion
// ─────────────────────────────────────────────────────────────

#[test]
fn t9_memory_qdrant_promoted_to_storage() {
    let cfg = v3_config();
    let qdrant = cfg
        .storage
        .qdrant
        .get("default")
        .expect("[memory.qdrant] promoted to [storage.qdrant.default]");
    assert_eq!(qdrant.url.as_deref(), Some("http://localhost:6333"));
    assert_eq!(qdrant.collection, "zc_memories");
    assert_eq!(qdrant.api_key.as_deref(), Some("qdrant-api-key"));
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
    let cfg = v3_config();
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
    let cfg = v3_config();
    let pg = cfg
        .storage
        .postgres
        .get("default")
        .expect("[storage.postgres.default] exists");
    // Connection fields from [storage.provider.config] (provider=postgres)
    // merge with vector fields from [memory.postgres] on the same entry.
    assert_eq!(
        pg.db_url.as_deref(),
        Some("postgres://user:pass@localhost/zc"),
        "V2 [storage.provider.config].db_url must land at V3 storage.postgres.default.db_url"
    );
    assert_eq!(pg.schema, "zeroclaw");
    assert_eq!(pg.table, "memories");
    assert_eq!(pg.connect_timeout_secs, Some(30));
}

// ─────────────────────────────────────────────────────────────
// T11 — cron job id drop + alias-keyed cron
// ─────────────────────────────────────────────────────────────

#[test]
fn t11_cron_job_id_dropped_and_alias_keyed() {
    let cfg = v3_config();
    let job = cfg
        .cron
        .get("morning_digest")
        .expect("cron job alias derived from name slug");
    // V2 had `id: String` on CronJobDecl; V3 removed it. The migrated job
    // table must not carry an `id` field — assert via raw value navigation
    // since V3 CronJobDecl doesn't even have a slot for it.
    let value = v3_value();
    let raw_job = value
        .get("cron")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("morning_digest"))
        .and_then(toml::Value::as_table)
        .expect("cron.morning_digest in raw migrated TOML");
    assert!(
        !raw_job.contains_key("id"),
        "V2 CronJobDecl.id must be dropped during V2→V3 cron restructure"
    );
    // Job content survives.
    assert_eq!(job.name.as_deref(), Some("Morning Digest"));
    assert_eq!(job.prompt.as_deref(), Some("Summarize unread messages"));
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
    let cfg = v3_config();
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
fn t13_security_resources_folded_into_risk_profile() {
    let cfg = v3_config();
    let profile = cfg
        .risk_profiles
        .get("default")
        .expect("risk_profiles.default present");
    assert_eq!(profile.max_memory_mb, 512);
    assert_eq!(profile.max_cpu_time_seconds, 600);
    assert_eq!(profile.max_subprocesses, 10);
    assert!(profile.memory_monitoring);
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
    let profile = cfg
        .runtime_profiles
        .get("agent_complex_agent")
        .expect("synthesized runtime_profiles.agent_complex_agent");
    assert!(profile.agentic);
    assert_eq!(profile.allowed_tools, vec!["shell", "memory"]);
    assert_eq!(profile.timeout_secs, Some(180));
    assert_eq!(profile.agentic_timeout_secs, Some(600));
}

#[test]
fn t14c_max_depth_synthesizes_per_agent_risk_profile() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.risk_profile, "agent_complex_agent",
        "V2 max_depth must point agent at synthesized per-agent risk profile"
    );
    let profile = cfg
        .risk_profiles
        .get("agent_complex_agent")
        .expect("synthesized risk_profiles.agent_complex_agent");
    assert_eq!(profile.max_delegation_depth, 4);
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

    // The bundle entry exists with the V2 directory captured.
    let bundle = cfg
        .skill_bundles
        .get("agent_complex_agent")
        .expect("skill_bundles.agent_complex_agent synthesized from V2 skills_directory");
    assert_eq!(
        bundle.directory.as_deref(),
        Some("/opt/zeroclaw/skills"),
        "V2 skills_directory value must land at skill_bundles.agent_<id>.directory"
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

#[test]
fn t14e_memory_namespace_widening() {
    let cfg = v3_config();
    let agent = cfg
        .agents
        .get("complex_agent")
        .expect("agents.complex_agent present");
    assert_eq!(
        agent.memory_namespace, "complex",
        "V2 Option<String> memory_namespace must widen to V3 String unchanged"
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
    assert_eq!(profile.shell_timeout_secs, 60);
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
        .get("anthropic")
        .and_then(|m| m.get("default"))
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
    let value = v3_value();
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
    // should round-trip through the toml_edit::DocumentMut reconciliation.
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
// V1/V2 colon-URL provider strings — `(custom|anthropic-custom):<url>`.
// Pre-fix the migration used the raw colon-URL string as the V3 outer
// provider key, then synthesized `model_provider = "<type>:<url>.<alias>"`.
// V3's `split_once('.')` resolution then tokenized at the first URL dot
// (e.g. inside `api.z.ai`), making the reference unresolvable. The fix
// splits the URL into `base_url` on the alias entry and uses only the
// prefix as the V3 type key, keeping `<type>.<alias>` parseable (#6266
// review).
// ─────────────────────────────────────────────────────────────

#[test]
fn anthropic_custom_colon_url_default_provider_splits_into_base_url() {
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
        .get("anthropic-custom")
        .and_then(|m| m.get("default"))
        .expect(
            "V3 outer key must be the dot-free prefix `anthropic-custom`, not the raw colon-URL string",
        );
    assert_eq!(
        entry.base_url.as_deref(),
        Some("https://api.z.ai/api/anthropic"),
        "the URL portion of the V2 colon-URL form must land in base_url on the alias entry"
    );
    assert_eq!(entry.model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(entry.api_key.as_deref(), Some("sk-zai-test"));
}

#[test]
fn custom_colon_url_default_provider_splits_into_base_url() {
    let raw = r#"
default_provider = "custom:http://localhost:8080/v1"
default_model = "local-model"
"#;
    let cfg =
        migrate_to_current(raw).expect("migration succeeds for plain `custom:` colon-URL form");
    let entry = cfg
        .providers
        .models
        .get("custom")
        .and_then(|m| m.get("default"))
        .expect("V3 outer key must be `custom`, not the raw colon-URL");
    assert_eq!(
        entry.base_url.as_deref(),
        Some("http://localhost:8080/v1"),
        "the URL portion of the V2 colon-URL form must land in base_url"
    );
    assert_eq!(entry.model.as_deref(), Some("local-model"));
}

#[test]
fn agent_inline_brain_colon_url_provider_splits_into_base_url() {
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

    let synthesized = v3
        .get("providers")
        .and_then(|v| v.get("models"))
        .and_then(|v| v.get("anthropic-custom"))
        .and_then(|v| v.get("agent_researcher"))
        .expect("providers.models.anthropic-custom.agent_researcher synthesized");
    assert_eq!(
        synthesized.get("base_url").and_then(toml::Value::as_str),
        Some("https://api.z.ai/api/anthropic"),
        "the colon-URL's URL portion must land in base_url on the synthesized alias entry"
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
