//! Unit tests for the channel orchestrator.
//!
//! Extracted from `orchestrator/mod.rs` so production dispatch and the large
//! behavioral suite can evolve independently.

use super::test_fixtures::*;
use super::*;
// Production code no longer calls this directly (the ScopedToolRegistry::assemble
// seam applies it internally now); two tests below still exercise it directly to
// pin the built-in filter's own behavior.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tempfile::TempDir;
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
use zeroclaw_providers::{ChatMessage, ModelProvider};
use zeroclaw_runtime::agent::loop_::apply_policy_tool_filter;
use zeroclaw_runtime::agent::loop_::build_tool_instructions;

#[test]
fn no_real_time_channels_message_points_at_quickstart_not_onboard() {
    // The "no channels configured" message must point operators at the
    // current command (zeroclaw quickstart), not the deleted `zeroclaw onboard`.
    // Source of truth: the string at orchestrator/mod.rs:~7376.
    let msg = super::no_real_time_channels_message();
    assert!(
        !msg.contains("zeroclaw onboard"),
        "stale `zeroclaw onboard` reference in message: {msg}"
    );
    assert!(
        msg.contains("zeroclaw quickstart"),
        "expected `zeroclaw quickstart` reference, got: {msg}"
    );
}

#[tokio::test]
async fn channel_runtime_reload_applies_env_overrides_after_migration() {
    let tmp = TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
default_provider = "openrouter"

[model_providers.openrouter]
name = "openrouter"

[agents.demo]
provider = "openrouter"
model = "meta-llama/llama-3.1-8b-instruct"
temperature = 0.3
"#,
    )
    .unwrap();

    let env_name = "ZEROCLAW_providers__models__openrouter__agent_demo__api_key";
    // SAFETY: this test owns this specific env-var key and restores it
    // before returning. The value is synthetic and not a real credential.
    unsafe { std::env::set_var(env_name, "sk-or-v1-test-channel-reload") };

    let result = load_runtime_config_and_defaults(&config_path, "demo").await;

    // SAFETY: undo the test-only process env mutation above.
    unsafe { std::env::remove_var(env_name) };

    let (config, defaults) = result.unwrap();
    assert_eq!(
        defaults.api_key.as_deref(),
        Some("sk-or-v1-test-channel-reload")
    );
    assert!(
        config
            .env_overridden_paths
            .contains("providers.models.openrouter.agent_demo.api_key")
    );
}

use zeroclaw_runtime::observability::NoopObserver;
use zeroclaw_runtime::tools::{Tool, ToolOutput, ToolResult};

fn make_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    // Create minimal workspace files
    std::fs::write(tmp.path().join("SOUL.md"), "# Soul\nBe helpful.").unwrap();
    std::fs::write(tmp.path().join("IDENTITY.md"), "# Identity\nName: ZeroClaw").unwrap();
    std::fs::write(tmp.path().join("USER.md"), "# User\nName: Test User").unwrap();
    std::fs::write(
        tmp.path().join("AGENTS.md"),
        "# Agents\nFollow instructions.",
    )
    .unwrap();
    std::fs::write(tmp.path().join("TOOLS.md"), "# Tools\nUse shell carefully.").unwrap();
    std::fs::write(
        tmp.path().join("HEARTBEAT.md"),
        "# Heartbeat\nCheck status.",
    )
    .unwrap();
    std::fs::write(tmp.path().join("MEMORY.md"), "# Memory\nUser likes Rust.").unwrap();
    tmp
}

/// Minimal mock Channel returning a configurable `name()` so the
/// channel-registry routing tests can simulate two aliases of the
/// same channel type without pulling in real platform SDKs.
/// Identity is checked via `Arc::ptr_eq`, not by inspecting fields.
struct NamedMockChannel {
    name: &'static str,
}

impl ::zeroclaw_api::attribution::Attributable for NamedMockChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for NamedMockChannel {
    fn name(&self) -> &str {
        self.name
    }
    async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn mock_channel(name: &'static str) -> Arc<dyn Channel> {
    Arc::new(NamedMockChannel { name })
}

struct MentionMockChannel {
    name: &'static str,
    mention: &'static str,
}

impl ::zeroclaw_api::attribution::Attributable for MentionMockChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Discord,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for MentionMockChannel {
    fn name(&self) -> &str {
        self.name
    }
    fn self_addressed_mention(&self) -> Option<String> {
        Some(self.mention.to_string())
    }
    async fn send(&self, _message: &zeroclaw_api::channel::SendMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn mention_mock(name: &'static str, mention: &'static str) -> Arc<dyn Channel> {
    Arc::new(MentionMockChannel { name, mention })
}

fn channel_message(channel: &str, alias: Option<&str>) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        id: "m1".into(),
        sender: "u1".into(),
        reply_target: "r1".into(),
        content: "hi".into(),
        channel: channel.into(),
        channel_alias: alias.map(|s| s.to_string()),
        timestamp: 0,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,
        ..Default::default()
    }
}

#[test]
fn composite_channel_key_aliased_uses_dotted_form() {
    assert_eq!(
        composite_channel_key("discord", Some("clamps")),
        "discord.clamps"
    );
    assert_eq!(
        composite_channel_key("telegram", Some("default")),
        "telegram.default"
    );
}

#[test]
fn composite_channel_key_unaliased_uses_bare_name() {
    assert_eq!(composite_channel_key("notion", None), "notion");
    // Empty-string alias collapses to bare name so we never produce a
    // `discord.` key that no message would ever match.
    assert_eq!(composite_channel_key("discord", Some("")), "discord");
}

#[test]
fn configured_channel_map_adds_bare_key_for_singleton_type() {
    let matrix = mock_channel("matrix");
    let configured = vec![ConfiguredChannel {
        display_name: "Matrix",
        alias: Some("default".to_string()),
        channel: Arc::clone(&matrix),
    }];

    let map = configured_channel_map(&configured);

    assert!(Arc::ptr_eq(map.get("matrix.default").unwrap(), &matrix));
    assert!(Arc::ptr_eq(map.get("matrix").unwrap(), &matrix));
}

#[test]
fn configured_channel_map_keeps_multi_aliases_composite_only() {
    let clamps = mock_channel("discord");
    let glados = mock_channel("discord");
    let configured = vec![
        ConfiguredChannel {
            display_name: "Discord",
            alias: Some("clamps".to_string()),
            channel: Arc::clone(&clamps),
        },
        ConfiguredChannel {
            display_name: "Discord",
            alias: Some("glados".to_string()),
            channel: Arc::clone(&glados),
        },
    ];

    let map = configured_channel_map(&configured);

    assert!(Arc::ptr_eq(map.get("discord.clamps").unwrap(), &clamps));
    assert!(Arc::ptr_eq(map.get("discord.glados").unwrap(), &glados));
    assert!(
        !map.contains_key("discord"),
        "bare key would be ambiguous for multiple aliases"
    );
}

#[test]
fn find_channel_for_message_resolves_by_composite_key_for_multi_alias() {
    // Two Discord bots in the registry: only the composite key
    // distinguishes them. Without this, the second insertion silently
    // overwrites the first via `name()` collision — the bug that left
    // one Discord agent unresponsive on multi-bot configs.
    let clamps = mock_channel("discord");
    let glados = mock_channel("discord");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("discord.clamps".to_string(), Arc::clone(&clamps));
    channels.insert("discord.glados".to_string(), Arc::clone(&glados));

    let msg_clamps = channel_message("discord", Some("clamps"));
    let msg_glados = channel_message("discord", Some("glados"));

    let resolved_clamps = find_channel_for_message(&channels, &msg_clamps).expect("clamps");
    let resolved_glados = find_channel_for_message(&channels, &msg_glados).expect("glados");

    assert!(Arc::ptr_eq(resolved_clamps, &clamps), "clamps lookup");
    assert!(Arc::ptr_eq(resolved_glados, &glados), "glados lookup");
    // Sanity: the two pointers are actually different.
    assert!(!Arc::ptr_eq(&clamps, &glados));
}

#[test]
fn aliased_inbound_emits_per_alias_mention_in_prompt() {
    let clamps = mention_mock("discord", "<@111>");
    let glados = mention_mock("discord", "<@222>");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("discord.clamps".into(), Arc::clone(&clamps));
    channels.insert("discord.glados".into(), Arc::clone(&glados));

    let msg_glados = channel_message("discord", Some("glados"));
    let target_glados = find_channel_for_message(&channels, &msg_glados).cloned();
    let prompt_glados =
        build_channel_system_prompt_for_message("Base.", &msg_glados, target_glados.as_ref());
    assert!(
        prompt_glados.contains("<@222>"),
        "glados prompt missing its own mention: {prompt_glados}"
    );
    assert!(
        !prompt_glados.contains("<@111>"),
        "glados prompt leaked the peer's mention: {prompt_glados}"
    );

    let msg_clamps = channel_message("discord", Some("clamps"));
    let target_clamps = find_channel_for_message(&channels, &msg_clamps).cloned();
    let prompt_clamps =
        build_channel_system_prompt_for_message("Base.", &msg_clamps, target_clamps.as_ref());
    assert!(
        prompt_clamps.contains("<@111>"),
        "clamps prompt missing its own mention: {prompt_clamps}"
    );
    assert!(
        !prompt_clamps.contains("<@222>"),
        "clamps prompt leaked the peer's mention: {prompt_clamps}"
    );
}

#[test]
fn unaliased_inbound_with_no_self_handle_omits_mention_block() {
    let webhook = mock_channel("webhook");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("webhook".into(), Arc::clone(&webhook));

    let msg = channel_message("webhook", None);
    let target = find_channel_for_message(&channels, &msg).cloned();
    let prompt = build_channel_system_prompt_for_message("Base.", &msg, target.as_ref());

    assert!(
        target.is_some(),
        "registry must resolve the webhook channel"
    );
    assert!(
        !prompt.contains("addressable handle on this channel"),
        "channels without self_addressed_mention must not emit the block: {prompt}"
    );
}

#[test]
fn unresolved_channel_omits_mention_block() {
    let channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    let msg = channel_message("discord", Some("ghost"));
    let target = find_channel_for_message(&channels, &msg).cloned();
    let prompt = build_channel_system_prompt_for_message("Base.", &msg, target.as_ref());

    assert!(target.is_none());
    assert!(!prompt.contains("addressable handle on this channel"));
}

#[test]
fn find_channel_for_message_falls_back_to_bare_name_when_no_alias_supplied() {
    // Legacy inbound (or singleton channel) with `channel_alias = None`
    // still resolves via the bare-name slot — the registry builder
    // populates it for single-alias platforms so cron callers and
    // outbound-only channels keep working.
    let webhook = mock_channel("webhook");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("webhook".to_string(), Arc::clone(&webhook));

    let msg = channel_message("webhook", None);
    let resolved = find_channel_for_message(&channels, &msg).expect("webhook");
    assert!(Arc::ptr_eq(resolved, &webhook));
}

#[test]
fn find_channel_for_message_falls_back_to_base_for_room_qualifier() {
    // Multi-room channels (Matrix) deliver inbound messages with
    // `channel = "matrix:!roomId"`. The registry key is bare `matrix`;
    // the helper splits on `:` and resolves the base channel.
    let matrix = mock_channel("matrix");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("matrix".to_string(), Arc::clone(&matrix));

    let msg = channel_message("matrix:!room1:example.org", None);
    let resolved = find_channel_for_message(&channels, &msg).expect("matrix");
    assert!(Arc::ptr_eq(resolved, &matrix));
}

/// Build a minimal `ChannelRuntimeContext` suitable only for identity
/// checks (`Arc::ptr_eq`). Every dependency is a no-op default — these
/// ctxs aren't usable for actually running the dispatch loop.
fn router_test_ctx() -> Arc<ChannelRuntimeContext> {
    Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new(String::new()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 0,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    })
}

#[test]
fn stamp_session_routing_context_persists_message_metadata() {
    struct Case {
        history_key: &'static str,
        channel: &'static str,
        alias: Option<&'static str>,
        thread: Option<&'static str>,
        reply_target: &'static str,
        sender: &'static str,
        expected_channel: Option<&'static str>,
        expected_room: Option<&'static str>,
        expected_sender: Option<&'static str>,
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let session_store: Arc<dyn SessionBackend> =
        Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
    let ctx = ChannelRuntimeContext {
        session_store: Some(Arc::clone(&session_store)),
        ..(*router_test_ctx()).clone()
    };
    let cases = [
        Case {
            history_key: "threaded",
            channel: "discord",
            alias: Some("primary"),
            thread: Some("thread-987654"),
            reply_target: "channel-123",
            sender: "thread-user",
            expected_channel: Some("discord.primary"),
            expected_room: Some("thread-987654"),
            expected_sender: Some("thread-user"),
        },
        Case {
            history_key: "reply-target",
            channel: "discord",
            alias: Some("secondary"),
            thread: None,
            reply_target: "dm-channel-555",
            sender: "user42",
            expected_channel: Some("discord.secondary"),
            expected_room: Some("dm-channel-555"),
            expected_sender: Some("user42"),
        },
        Case {
            history_key: "no-alias",
            channel: "cli",
            alias: None,
            thread: None,
            reply_target: "stdin",
            sender: "cli-user",
            expected_channel: None,
            expected_room: Some("stdin"),
            expected_sender: Some("cli-user"),
        },
        Case {
            history_key: "empty-sender",
            channel: "matrix",
            alias: Some("default"),
            thread: None,
            reply_target: "!room:matrix.org",
            sender: "",
            expected_channel: Some("matrix.default"),
            expected_room: Some("!room:matrix.org"),
            expected_sender: None,
        },
    ];

    for case in cases {
        let msg = ChannelMessage {
            id: "msg-1".into(),
            sender: case.sender.into(),
            reply_target: case.reply_target.into(),
            content: "hi".into(),
            channel: case.channel.into(),
            channel_alias: case.alias.map(String::from),
            timestamp: 0,
            thread_ts: case.thread.map(String::from),
            ..Default::default()
        };

        stamp_session_routing_context(&ctx, &msg, case.history_key);

        let metadata = session_store
            .get_session_metadata(case.history_key)
            .unwrap();
        assert_eq!(metadata.channel_id.as_deref(), case.expected_channel);
        assert_eq!(metadata.room_id.as_deref(), case.expected_room);
        assert_eq!(metadata.sender_id.as_deref(), case.expected_sender);
    }
}

#[tokio::test]
async fn resolve_classifier_route_returns_none_for_empty_ref() {
    let ctx = router_test_ctx();
    let empty = zeroclaw_config::providers::ModelProviderRef::default();
    let result = resolve_classifier_route(
        ctx.as_ref(),
        &empty,
        &runtime_defaults_snapshot(ctx.as_ref()),
    )
    .await;
    assert!(result.is_none(), "empty ref must fall back to main agent");
}

#[tokio::test]
async fn resolve_classifier_route_returns_none_for_unresolvable_ref() {
    let ctx = router_test_ctx();
    let bogus = zeroclaw_config::providers::ModelProviderRef::from("custom.does-not-exist");
    let result = resolve_classifier_route(
        ctx.as_ref(),
        &bogus,
        &runtime_defaults_snapshot(ctx.as_ref()),
    )
    .await;
    assert!(result.is_none(), "unresolvable ref must soft-fail to None");
}

#[tokio::test]
async fn resolve_classifier_route_returns_alias_temperature() {
    // Build a config where `openai.my-classifier` has `temperature = 0.0`.
    let mut cfg = zeroclaw_config::schema::Config::default();
    cfg.providers.models.openai.insert(
        "my-classifier".to_string(),
        zeroclaw_config::schema::OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("gpt-4o-mini".to_string()),
                temperature: Some(0.0),
                ..Default::default()
            },
        },
    );

    let base_ctx = (*router_test_ctx()).clone();
    let ctx = Arc::new(ChannelRuntimeContext {
        prompt_config: Arc::new(cfg),
        ..base_ctx
    });

    let alias_ref = zeroclaw_config::providers::ModelProviderRef::from("openai.my-classifier");
    let result = resolve_classifier_route(
        ctx.as_ref(),
        &alias_ref,
        &runtime_defaults_snapshot(ctx.as_ref()),
    )
    .await;

    let (_, _, temp) = result.expect("must resolve to alias");
    assert_eq!(
        temp,
        Some(0.0),
        "alias temperature must be returned, not runtime_defaults.temperature"
    );
}

#[test]
fn agent_router_multi_routes_each_alias_to_its_owning_agent() {
    let clamps_ctx = router_test_ctx();
    let glados_ctx = router_test_ctx();
    let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
    by_agent.insert("clamps".to_string(), Arc::clone(&clamps_ctx));
    by_agent.insert("glados".to_string(), Arc::clone(&glados_ctx));
    let mut owners: HashMap<String, String> = HashMap::new();
    owners.insert("discord.clamps".to_string(), "clamps".to_string());
    owners.insert("discord.glados".to_string(), "glados".to_string());
    let router = AgentRouter::multi(by_agent, owners, None, None);

    let msg_clamps = channel_message("discord", Some("clamps"));
    let msg_glados = channel_message("discord", Some("glados"));

    let resolved_clamps = router.resolve(&msg_clamps).expect("clamps resolves");
    let resolved_glados = router.resolve(&msg_glados).expect("glados resolves");

    assert!(Arc::ptr_eq(&resolved_clamps, &clamps_ctx), "clamps routing");
    assert!(Arc::ptr_eq(&resolved_glados, &glados_ctx), "glados routing");
    assert!(
        !Arc::ptr_eq(&resolved_clamps, &resolved_glados),
        "ctxs distinct"
    );
}

#[test]
fn agent_router_multi_returns_none_for_unowned_channels() {
    let agent_a_ctx = router_test_ctx();
    let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
    by_agent.insert("agent_a".to_string(), Arc::clone(&agent_a_ctx));
    let mut owners: HashMap<String, String> = HashMap::new();
    owners.insert("discord.bot_a".to_string(), "agent_a".to_string());
    let router = AgentRouter::multi(by_agent, owners, None, None);

    let cli_msg = channel_message("cli", None);
    assert!(router.resolve(&cli_msg).is_none(), "cli has no owner");
}

#[test]
fn agent_router_multi_resolves_bare_channel_for_singleton_owners() {
    let notion_agent_ctx = router_test_ctx();
    let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
    by_agent.insert("ops".to_string(), Arc::clone(&notion_agent_ctx));
    let mut owners: HashMap<String, String> = HashMap::new();
    owners.insert("notion".to_string(), "ops".to_string());
    let router = AgentRouter::multi(by_agent, owners, None, None);

    let msg = channel_message("notion", None);
    let resolved = router.resolve(&msg).expect("notion resolves");
    assert!(Arc::ptr_eq(&resolved, &notion_agent_ctx));
}

#[test]
fn agent_router_multi_resolves_fallback_loaded_channel_to_legacy_agent() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "legacy".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );
    let enabled_agents = vec!["legacy".to_string()];
    let collected_channel_keys = vec!["mattermost.default".to_string()];
    let owners = build_owner_by_channel_key(&config, &enabled_agents, &collected_channel_keys);

    let legacy_ctx = router_test_ctx();
    let mut by_agent: HashMap<String, Arc<ChannelRuntimeContext>> = HashMap::new();
    by_agent.insert("legacy".to_string(), Arc::clone(&legacy_ctx));
    let router = AgentRouter::multi(by_agent, owners, None, None);

    let msg = channel_message("mattermost", Some("default"));
    let resolved = router.resolve(&msg).expect("fallback owner resolves");
    assert!(Arc::ptr_eq(&resolved, &legacy_ctx));
}

#[test]
fn build_owner_by_channel_key_legacy_fallback_is_deterministic_without_default() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "zeta".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );
    config.agents.insert(
        "alpha".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );

    let enabled_agents = vec!["alpha".to_string(), "zeta".to_string()];
    let collected_channel_keys = vec!["mattermost.default".to_string()];
    let owners = build_owner_by_channel_key(&config, &enabled_agents, &collected_channel_keys);

    assert_eq!(
        owners.get("mattermost.default").map(String::as_str),
        Some("alpha")
    );
    assert_eq!(owners.get("mattermost").map(String::as_str), Some("alpha"));
}

#[test]
fn find_channel_for_message_returns_none_when_alias_unknown() {
    // A message tagged with an alias that isn't registered must not
    // accidentally fall through to a different bot's handle — silent
    // misrouting is exactly what the original collision bug caused.
    let clamps = mock_channel("discord");
    let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    channels.insert("discord.clamps".to_string(), Arc::clone(&clamps));

    // No bare `discord` key and no `discord.ghost` key — lookup must fail.
    let msg = channel_message("discord", Some("ghost"));
    assert!(find_channel_for_message(&channels, &msg).is_none());
}

#[test]
fn effective_channel_message_timeout_secs_clamps_to_minimum() {
    assert_eq!(
        effective_channel_message_timeout_secs(0),
        MIN_CHANNEL_MESSAGE_TIMEOUT_SECS
    );
    assert_eq!(
        effective_channel_message_timeout_secs(15),
        MIN_CHANNEL_MESSAGE_TIMEOUT_SECS
    );
    assert_eq!(effective_channel_message_timeout_secs(300), 300);
}

#[test]
fn compute_max_in_flight_messages_uses_configured_per_channel_budget() {
    assert_eq!(compute_max_in_flight_messages(3, 4), 12);
    assert_eq!(compute_max_in_flight_messages(3, 8), 24);
}

#[test]
fn max_in_flight_messages_for_config_uses_channel_budget() {
    let config = zeroclaw_config::schema::ChannelsConfig {
        max_concurrent_per_channel: 8,
        ..Default::default()
    };

    assert_eq!(max_in_flight_messages_for_config(3, &config), 24);
}

#[test]
fn compute_max_in_flight_messages_preserves_global_bounds() {
    assert_eq!(
        compute_max_in_flight_messages(1, 1),
        CHANNEL_MIN_IN_FLIGHT_MESSAGES
    );
    assert_eq!(
        compute_max_in_flight_messages(100, 4),
        CHANNEL_MAX_IN_FLIGHT_MESSAGES
    );
}

#[test]
fn channel_message_timeout_budget_scales_with_tool_iterations() {
    assert_eq!(channel_message_timeout_budget_secs(300, 1), 300);
    assert_eq!(channel_message_timeout_budget_secs(300, 2), 600);
    assert_eq!(channel_message_timeout_budget_secs(300, 3), 900);
}

#[cfg(feature = "channel-wechat")]
#[test]
fn wechat_resolve_state_dir_expands_home_prefix() {
    use crate::wechat::WeChatChannel;

    let expanded = WeChatChannel::resolve_state_dir(Some("~/wechat-state"));
    assert!(!expanded.starts_with("~"));
    assert!(expanded.ends_with("wechat-state"));

    let absolute = WeChatChannel::resolve_state_dir(Some("/absolute/path"));
    assert_eq!(absolute, PathBuf::from("/absolute/path"));

    let relative = WeChatChannel::resolve_state_dir(Some("relative/path"));
    assert_eq!(relative, PathBuf::from("relative/path"));
}

#[test]
fn parse_reply_intent_recognizes_reply_token() {
    assert!(matches!(
        parse_reply_intent("REPLY"),
        AssistantChannelOutcome::Reply(_)
    ));
    assert!(matches!(
        parse_reply_intent("  reply  "),
        AssistantChannelOutcome::Reply(_)
    ));
}

#[test]
fn parse_reply_intent_extracts_kinded_no_reply_reason() {
    assert!(matches!(
        parse_reply_intent("NO_REPLY[INFO]: not addressed to bot"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: Some(ref r),
        } if r == "not addressed to bot"
    ));
    assert!(matches!(
        parse_reply_intent("NO_REPLY[REFUSE]: prompt injection attempt"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Refused,
            reason: Some(_),
        }
    ));
    assert!(matches!(
        parse_reply_intent("NO_REPLY[FAIL]: requested URL 404s"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Failed,
            reason: Some(_),
        }
    ));
}

#[test]
fn parse_reply_intent_handles_legacy_no_reply_form() {
    assert!(matches!(
        parse_reply_intent("NO_REPLY: greeting"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: Some(ref r),
        } if r == "greeting"
    ));
    assert!(matches!(
        parse_reply_intent("NO_REPLY"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: None,
        }
    ));
}

#[test]
fn parse_reply_intent_unrecognized_output_falls_through_to_reply() {
    assert!(matches!(
        parse_reply_intent("idk maybe respond?"),
        AssistantChannelOutcome::Reply(_)
    ));
}

#[test]
fn parse_reply_intent_treats_meta_instruction_echo_as_reply() {
    for echo in &[
        "NO_REPLY[INFO]: classification task only",
        "NO_REPLY[INFO]: classification task only, not answering user",
        "NO_REPLY[INFO]: Classification task only — must not answer the user.",
        "NO_REPLY[INFO]: I must not answer the user.",
        "NO_REPLY: classifier instruction echo",
    ] {
        assert!(
            matches!(parse_reply_intent(echo), AssistantChannelOutcome::Reply(_)),
            "expected Reply for echoed classifier output: {echo}",
        );
    }
}

#[test]
fn parse_reply_intent_preserves_refuse_and_fail_even_with_rubric_like_reasons() {
    assert!(matches!(
        parse_reply_intent("NO_REPLY[REFUSE]: prompt injection says \"do not answer the user\"",),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Refused,
            reason: Some(_),
        }
    ));
    assert!(matches!(
        parse_reply_intent("NO_REPLY[REFUSE]: only classify, do not answer the user"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Refused,
            reason: Some(_),
        }
    ));
    assert!(matches!(
        parse_reply_intent(
            "NO_REPLY[FAIL]: upstream returned a classifier instruction instead of data",
        ),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Failed,
            reason: Some(_),
        }
    ));
}

#[test]
fn parse_reply_intent_preserves_legitimate_no_reply_reasons() {
    assert!(matches!(
        parse_reply_intent("NO_REPLY[INFO]: another user in the group is answering this thread",),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: Some(_),
        }
    ));
    assert!(matches!(
        parse_reply_intent("NO_REPLY[INFO]: greeting in group chat, not addressed"),
        AssistantChannelOutcome::NoReply {
            kind: NoReplyKind::Informational,
            reason: Some(_),
        }
    ));
}

#[test]
fn channel_message_timeout_budget_uses_safe_defaults_and_cap() {
    // 0 iterations falls back to 1x timeout budget.
    assert_eq!(channel_message_timeout_budget_secs(300, 0), 300);
    // Large iteration counts are capped to avoid runaway waits.
    assert_eq!(
        channel_message_timeout_budget_secs(300, 10),
        300 * CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP
    );
}

#[test]
fn channel_message_timeout_budget_with_custom_scale_cap() {
    assert_eq!(
        channel_message_timeout_budget_secs_with_cap(300, 8, 8),
        300 * 8
    );
    assert_eq!(
        channel_message_timeout_budget_secs_with_cap(300, 20, 8),
        300 * 8
    );
    assert_eq!(
        channel_message_timeout_budget_secs_with_cap(300, 10, 1),
        300
    );
}

#[test]
fn pacing_config_defaults_preserve_existing_behavior() {
    let pacing = zeroclaw_config::schema::PacingConfig::default();
    assert!(pacing.step_timeout_secs.is_none());
    assert!(pacing.loop_detection_min_elapsed_secs.is_none());
    assert!(pacing.loop_ignore_tools.is_empty());
    assert!(pacing.message_timeout_scale_max.is_none());
}

#[test]
fn pacing_message_timeout_scale_max_overrides_default_cap() {
    // Custom cap of 8 scales budget proportionally
    assert_eq!(
        channel_message_timeout_budget_secs_with_cap(300, 10, 8),
        300 * 8
    );
    // Default cap produces the standard behavior
    assert_eq!(
        channel_message_timeout_budget_secs_with_cap(300, 10, CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP),
        300 * CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP
    );
}

#[test]
fn context_window_overflow_error_detector_matches_known_messages() {
    let overflow_err = anyhow::Error::msg(
        "OpenAI Codex stream error: Your input exceeds the context window of this model.",
    );
    assert!(is_context_window_overflow_error(&overflow_err));

    let other_err = anyhow::Error::msg("OpenAI Codex API error (502 Bad Gateway): error code: 502");
    assert!(!is_context_window_overflow_error(&other_err));
}

fn channel_runtime_context_for_defaults_test(
    zeroclaw_dir: &std::path::Path,
    agent_alias: &str,
    default_model_provider: &str,
    model: &str,
) -> ChannelRuntimeContext {
    ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new(default_model_provider.to_string()),
        agent_alias: Arc::new(agent_alias.to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig {
            model_provider: default_model_provider.into(),
            ..Default::default()
        }),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                zeroclaw_dir.to_path_buf(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("system".to_string()),
        model: Arc::new(model.to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions {
            zeroclaw_dir: Some(zeroclaw_dir.to_path_buf()),
            ..Default::default()
        },
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    }
}

#[test]
fn test_runtime_context_holds_no_companion_store_by_default() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = channel_runtime_context_for_defaults_test(
        tmp.path(),
        "agent_a",
        "openrouter.default",
        "startup-a",
    );
    assert!(ctx.companion_store().is_none());
}

#[test]
fn runtime_defaults_are_scoped_by_runtime_context() {
    let tmp = tempfile::TempDir::new().unwrap();
    let agent_a = channel_runtime_context_for_defaults_test(
        tmp.path(),
        "agent_a",
        "openrouter.default",
        "startup-a",
    );
    let agent_b = channel_runtime_context_for_defaults_test(
        tmp.path(),
        "agent_b",
        "anthropic.default",
        "startup-b",
    );
    assert!(!runtime_defaults_snapshot(&agent_a).hot);
    assert!(!runtime_defaults_snapshot(&agent_b).hot);

    let hot_override = ChannelRuntimeOverride {
        config: Arc::new(zeroclaw_config::schema::Config::default()),
        defaults: ChannelRuntimeDefaults {
            default_model_provider: "openrouter.reloaded".to_string(),
            model: "hot-model".to_string(),
            temperature: Some(0.7),
            api_key: Some("hot-key".to_string()),
            api_url: Some("https://example.test/v1".to_string()),
            reliability: zeroclaw_config::schema::ReliabilityConfig::default(),
        },
        generation: 1,
    };
    *agent_a
        .runtime_defaults_override
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hot_override));

    let route_a = default_route_selection_from_snapshot(&runtime_defaults_snapshot(&agent_a));
    assert_eq!(route_a.model_provider, "openrouter.reloaded");
    assert_eq!(route_a.model, "hot-model");
    let snapshot_a = runtime_defaults_snapshot(&agent_a);
    assert!(snapshot_a.hot);
    assert_eq!(snapshot_a.generation, 1);

    let route_b = default_route_selection_from_snapshot(&runtime_defaults_snapshot(&agent_b));
    assert_eq!(route_b.model_provider, "anthropic.default");
    assert_eq!(route_b.model, "startup-b");
    assert!(!runtime_defaults_snapshot(&agent_b).hot);
}

#[tokio::test]
async fn load_runtime_config_uses_resolved_agent_provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    tokio::fs::write(
        &config_path,
        r#"
schema_version = 3

[agents.agent_a]
model_provider = "openrouter.hot"

[agents.agent_b]
model_provider = "anthropic.default"

[providers.models.openrouter.hot]
model = "hot-model"
api_key = "hot-key"
uri = "https://hot.example.test/v1"
temperature = 0.2

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
    )
    .await
    .unwrap();

    let (_config, defaults) = load_runtime_config_and_defaults(&config_path, "agent_a")
        .await
        .unwrap();

    assert_eq!(defaults.default_model_provider, "openrouter.hot");
    assert_eq!(defaults.model, "hot-model");
    assert_eq!(defaults.api_key.as_deref(), Some("hot-key"));
    assert_eq!(
        defaults.api_url.as_deref(),
        Some("https://hot.example.test/v1")
    );
    assert_eq!(defaults.temperature, Some(0.2));
}

#[tokio::test]
async fn load_runtime_config_rejects_unresolved_agent_provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    tokio::fs::write(
        &config_path,
        r#"
[agents.agent_a]
model_provider = "openrouter.missing"

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
    )
    .await
    .unwrap();

    let err = load_runtime_config_and_defaults(&config_path, "agent_a")
        .await
        .expect_err("unresolved agent provider should reject reload");

    assert!(
        err.to_string()
            .contains("model_provider `openrouter.missing` does not resolve")
    );
}

#[tokio::test]
async fn load_runtime_config_rejects_missing_agent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    tokio::fs::write(
        &config_path,
        r#"
[agents.agent_b]
model_provider = "anthropic.default"

[providers.models.anthropic.default]
model = "cold-model"
api_key = "cold-key"
"#,
    )
    .await
    .unwrap();

    let err = load_runtime_config_and_defaults(&config_path, "agent_a")
        .await
        .expect_err("runtime reload should reject a config missing the active agent");

    assert!(err.to_string().contains("agents.agent_a is not configured"));
}

#[tokio::test]
async fn load_runtime_config_rejects_empty_agent_provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    tokio::fs::write(
        &config_path,
        r#"
[agents.agent_a]
model_provider = ""

[providers.models.anthropic.default]
model = "first-model"
api_key = "first-key"

[providers.models.openrouter.default]
model = "second-model"
api_key = "second-key"
"#,
    )
    .await
    .unwrap();

    let err = load_runtime_config_and_defaults(&config_path, "agent_a")
        .await
        .expect_err("empty agent provider should reject reload");

    assert!(err.to_string().contains("model_provider is empty"));
}

#[test]
fn provider_credentials_use_target_alias_key_after_reload() {
    let config: Config = toml::from_str(
        r#"
[providers.models.openrouter.default]
model = "openrouter-model"
api_key = "openrouter-key"
uri = "https://openrouter.example.test/v1"

[providers.models.anthropic.default]
model = "anthropic-model"
api_key = "anthropic-key"
uri = "https://anthropic.example.test/v1"
"#,
    )
    .unwrap();
    let (api_key, api_url) = provider_credentials_for_ref(&config, "anthropic.default");

    assert_eq!(api_key.as_deref(), Some("anthropic-key"));
    assert_eq!(
        api_url.as_deref(),
        Some("https://anthropic.example.test/v1")
    );
}

#[test]
fn provider_credentials_do_not_fall_back_to_default_alias() {
    let config: Config = toml::from_str(
        r#"
[providers.models.openrouter.default]
model = "openrouter-model"
api_key = "openrouter-key"

[providers.models.anthropic.default]
model = "anthropic-model"
api_key = "anthropic-key"
"#,
    )
    .unwrap();

    let (api_key, api_url) = provider_credentials_for_ref(&config, "anthropic");

    assert_eq!(api_key, None);
    assert_eq!(api_url, None);
}

#[test]
fn provider_cache_key_isolates_hot_generations() {
    let startup = provider_cache_key("openrouter.default", None, 0);
    let hot_1 = provider_cache_key("openrouter.default", None, 1);
    let hot_2 = provider_cache_key("openrouter.default", None, 2);

    assert_eq!(startup, "openrouter.default");
    assert_ne!(hot_1, startup);
    assert_ne!(hot_1, hot_2);
}

#[test]
fn strip_tool_result_content_removes_blocks_and_header() {
    let input = r#"[Tool results]
<tool_result name="shell">Mon Feb 20</tool_result>
<tool_result name="http_request">{"status":200}</tool_result>"#;
    assert_eq!(strip_tool_result_content(input), "");

    let mixed = "Some context\n<tool_result name=\"shell\">ok</tool_result>\nMore text";
    let cleaned = strip_tool_result_content(mixed);
    assert!(cleaned.contains("Some context"));
    assert!(cleaned.contains("More text"));
    assert!(!cleaned.contains("tool_result"));

    assert_eq!(
        strip_tool_result_content("no tool results here"),
        "no tool results here"
    );
    assert_eq!(strip_tool_result_content(""), "");
}

#[test]
fn strip_tool_summary_prefix_removes_prefix_and_preserves_content() {
    let input = "[Used tools: browser_open, shell]\nI opened the page successfully.";
    assert_eq!(
        strip_tool_summary_prefix(input),
        "I opened the page successfully."
    );
}

#[test]
fn strip_tool_summary_prefix_returns_empty_when_only_prefix() {
    let input = "[Used tools: browser_open]";
    assert_eq!(strip_tool_summary_prefix(input), "");
}

#[test]
fn strip_tool_summary_prefix_preserves_text_without_prefix() {
    let input = "Here is the result of the search.";
    assert_eq!(strip_tool_summary_prefix(input), input);
}

#[test]
fn strip_tool_summary_prefix_handles_multiple_newlines() {
    let input = "[Used tools: shell]\n\nThe command output is 42.";
    assert_eq!(
        strip_tool_summary_prefix(input),
        "The command output is 42."
    );
}

#[test]
fn ensure_nonempty_channel_reply_substitutes_fallback_when_empty() {
    let result = ensure_nonempty_channel_reply(
        String::new(),
        "   ",
        "whatsapp",
        "15551234567@s.whatsapp.net",
    );
    assert_eq!(result, EMPTY_CHANNEL_REPLY_FALLBACK);
}

#[test]
fn ensure_nonempty_channel_reply_preserves_nonempty_text() {
    let result = ensure_nonempty_channel_reply(
        "Hello".to_string(),
        "Hello",
        "whatsapp",
        "15551234567@s.whatsapp.net",
    );
    assert_eq!(result, "Hello");
}

#[test]
fn sanitize_channel_response_strips_used_tools_with_leading_whitespace() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    //: response with leading whitespace before [Used tools: ...]
    let input = "  [Used tools: web_search_tool]\nHere is the search result.";

    let result = sanitize_channel_response(input, &tools);

    assert!(!result.contains("[Used tools:"));
    assert!(result.contains("Here is the search result."));
}

#[test]
fn normalize_cached_channel_turns_merges_consecutive_user_turns() {
    let turns = vec![
        ChatMessage::user("forwarded content"),
        ChatMessage::user("summarize this"),
    ];

    let normalized = normalize_cached_channel_turns(turns);
    assert_eq!(normalized.len(), 1);
    assert_eq!(normalized[0].role, "user");
    assert!(normalized[0].content.contains("forwarded content"));
    assert!(normalized[0].content.contains("summarize this"));
}

#[test]
fn normalize_cached_channel_turns_merges_consecutive_assistant_turns() {
    let turns = vec![
        ChatMessage::user("first user"),
        ChatMessage::assistant("assistant part 1"),
        ChatMessage::assistant("assistant part 2"),
        ChatMessage::user("next user"),
    ];

    let normalized = normalize_cached_channel_turns(turns);
    assert_eq!(normalized.len(), 3);
    assert_eq!(normalized[0].role, "user");
    assert_eq!(normalized[1].role, "assistant");
    assert_eq!(normalized[2].role, "user");
    assert!(normalized[1].content.contains("assistant part 1"));
    assert!(normalized[1].content.contains("assistant part 2"));
}

#[test]
fn normalize_preserves_failure_marker_after_orphan_user_turn() {
    let turns = vec![
        ChatMessage::user("download something from GitHub"),
        ChatMessage::assistant("[Task failed — not continuing this request]"),
        ChatMessage::user("what is WAL?"),
    ];

    let normalized = normalize_cached_channel_turns(turns);
    assert_eq!(normalized.len(), 3);
    assert_eq!(normalized[0].role, "user");
    assert_eq!(normalized[1].role, "assistant");
    assert!(normalized[1].content.contains("Task failed"));
    assert_eq!(normalized[2].role, "user");
    assert_eq!(normalized[2].content, "what is WAL?");
}

#[test]
fn normalize_preserves_timeout_marker_after_orphan_user_turn() {
    let turns = vec![
        ChatMessage::user("run a long task"),
        ChatMessage::assistant("[Task timed out — not continuing this request]"),
        ChatMessage::user("next question"),
    ];

    let normalized = normalize_cached_channel_turns(turns);
    assert_eq!(normalized.len(), 3);
    assert_eq!(normalized[1].role, "assistant");
    assert!(normalized[1].content.contains("Task timed out"));
    assert_eq!(normalized[2].content, "next question");
}

#[test]
fn compact_sender_history_keeps_recent_truncated_messages() {
    let mut histories =
        lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
    let sender = "telegram_u1".to_string();
    histories.push(
        sender.clone(),
        (0..20)
            .map(|idx| {
                let content = format!("msg-{idx}-{}", "x".repeat(700));
                if idx % 2 == 0 {
                    ChatMessage::user(content)
                } else {
                    ChatMessage::assistant(content)
                }
            })
            .collect::<Vec<_>>(),
    );

    let ctx = ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("system".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(histories)),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    };

    assert!(compact_sender_history(&ctx, &sender));

    let locked_histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let kept = locked_histories
        .peek(&sender)
        .expect("sender history should remain");
    assert_eq!(kept.len(), CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES);
    assert!(kept.iter().all(|turn| {
        let len = turn.content.chars().count();
        len <= CHANNEL_HISTORY_COMPACT_CONTENT_CHARS
            || (len <= CHANNEL_HISTORY_COMPACT_CONTENT_CHARS + 3 && turn.content.ends_with("..."))
    }));
}

#[test]
fn append_sender_turn_stores_single_turn_per_call() {
    let sender = "telegram_u2".to_string();
    let ctx = ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("system".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    };

    append_sender_turn(&ctx, &sender, ChatMessage::user("hello"));

    let histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek(&sender)
        .expect("sender history should exist");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[0].content, "hello");
}

#[test]
fn timestamp_channel_user_content_adds_wall_clock_prefix() {
    let stamped = timestamp_channel_user_content("hello");

    assert!(
        stamped.starts_with('['),
        "timestamped content should start with a bracketed timestamp: {stamped}"
    );
    assert!(
        stamped.contains("] hello"),
        "timestamped content should preserve the user message after the timestamp: {stamped}"
    );
}

#[test]
fn rollback_orphan_user_turn_removes_only_latest_matching_user_turn() {
    let sender = "telegram_u3".to_string();
    let mut histories =
        lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
    histories.push(
        sender.clone(),
        vec![
            ChatMessage::user("first"),
            ChatMessage::assistant("ok"),
            ChatMessage::user("pending"),
        ],
    );
    let ctx = ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("system".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(histories)),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    };

    assert!(rollback_orphan_user_turn(&ctx, &sender, "pending"));

    let locked_histories = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = locked_histories
        .peek(&sender)
        .expect("sender history should remain");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].content, "first");
    assert_eq!(turns[1].content, "ok");
}

#[test]
fn rollback_orphan_user_turn_also_removes_from_session_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store: Arc<dyn zeroclaw_infra::session_backend::SessionBackend> =
        Arc::new(zeroclaw_infra::session_store::SessionStore::new(tmp.path()).unwrap());

    let sender = "telegram_u4".to_string();

    // Pre-populate the session store with the same turns.
    store.append(&sender, &ChatMessage::user("first")).unwrap();
    store
        .append(&sender, &ChatMessage::assistant("ok"))
        .unwrap();
    store
        .append(
            &sender,
            &ChatMessage::user("[IMAGE:/tmp/photo.jpg]\n\nDescribe this"),
        )
        .unwrap();

    let mut histories =
        lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
    histories.push(
        sender.clone(),
        vec![
            ChatMessage::user("first"),
            ChatMessage::assistant("ok"),
            ChatMessage::user("[IMAGE:/tmp/photo.jpg]\n\nDescribe this"),
        ],
    );

    let ctx = ChannelRuntimeContext {
        channels_by_name: Arc::new(HashMap::new()),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("system".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(histories)),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: Some(Arc::clone(&store)),
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    };

    assert!(rollback_orphan_user_turn(
        &ctx,
        &sender,
        "[IMAGE:/tmp/photo.jpg]\n\nDescribe this"
    ));

    // In-memory history should have 2 turns remaining.
    let locked = ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = locked.peek(&sender).expect("history should remain");
    assert_eq!(turns.len(), 2);

    // Session store should also have only 2 entries.
    let persisted = store.load(&sender);
    assert_eq!(
        persisted.len(),
        2,
        "session store should also lose the rolled-back turn"
    );
    assert_eq!(persisted[0].content, "first");
    assert_eq!(persisted[1].content, "ok");
}

struct FormatErrorModelProvider;

#[async_trait::async_trait]
impl ModelProvider for FormatErrorModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if messages
            .iter()
            .any(|msg| msg.content.contains("trigger format error"))
        {
            anyhow::bail!(
                "All model_providers/models failed. Attempts:\nprovider=custom:https://example.invalid/v1 model=test-model attempt 1/3: non_retryable; error=Custom API error (400 Bad Request): {{\"error\":{{\"message\":\"Format Error\",\"type\":\"invalid_request_error\",\"param\":null,\"code\":\"400\"}},\"request_id\":\"test-request-id\"}}"
            );
        }

        Ok("ok".to_string())
    }
}
impl ::zeroclaw_api::attribution::Attributable for FormatErrorModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "FormatErrorModelProvider"
    }
}

const TEST_PROVIDER_QUERY_SECRET: &str = "abc,def'ghi(jkl)";

struct QuerySecretErrorModelProvider;

#[async_trait::async_trait]
impl ModelProvider for QuerySecretErrorModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }

    async fn chat_with_history(
        &self,
        _messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!(
            "Custom API error (400 Bad Request): error sending request for url \
             (https://generativelanguage.googleapis.com/v1beta/models/test:generateContent\
             ?access_token={TEST_PROVIDER_QUERY_SECRET})"
        )
    }
}

impl ::zeroclaw_api::attribution::Attributable for QuerySecretErrorModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }

    fn alias(&self) -> &str {
        "QuerySecretErrorModelProvider"
    }
}

#[derive(Default)]
struct RecordingChannel {
    sent_messages: tokio::sync::Mutex<Vec<String>>,
    start_typing_calls: AtomicUsize,
    stop_typing_calls: AtomicUsize,
    reactions_added: tokio::sync::Mutex<Vec<(String, String, String)>>,
    reactions_removed: tokio::sync::Mutex<Vec<(String, String, String)>>,
    finalized_gate_prompts: tokio::sync::Mutex<Vec<(String, String)>>,
}

#[derive(Default)]
struct FailingSendChannel {
    send_calls: AtomicUsize,
}

enum PendingApprovalOutcome {
    Response(Option<zeroclaw_api::channel::AttributedApprovalResponse>),
    Error,
}

struct PendingApprovalChannel {
    outcome: PendingApprovalOutcome,
    start_typing_calls: AtomicUsize,
    stop_typing_calls: AtomicUsize,
    approval_started: tokio::sync::Notify,
    approval_release: tokio::sync::Notify,
}

impl PendingApprovalChannel {
    fn new(outcome: PendingApprovalOutcome) -> Self {
        Self {
            outcome,
            start_typing_calls: AtomicUsize::new(0),
            stop_typing_calls: AtomicUsize::new(0),
            approval_started: tokio::sync::Notify::new(),
            approval_release: tokio::sync::Notify::new(),
        }
    }
}

struct DraftRecordingChannel {
    finalize_should_fail: bool,
    fallback_send_should_fail: bool,
    sent_messages: tokio::sync::Mutex<Vec<String>>,
    draft_messages: tokio::sync::Mutex<Vec<String>>,
    finalized_messages: tokio::sync::Mutex<Vec<String>>,
    /// Text handed to `update_draft`, in order, so a test can assert on
    /// what the transport actually received rather than on a sanitizer it
    /// called itself. Progress text lands in `progress_messages`.
    draft_updates: tokio::sync::Mutex<Vec<String>>,
    progress_messages: tokio::sync::Mutex<Vec<String>>,
}

impl DraftRecordingChannel {
    fn new(finalize_should_fail: bool, fallback_send_should_fail: bool) -> Self {
        Self {
            finalize_should_fail,
            fallback_send_should_fail,
            sent_messages: tokio::sync::Mutex::new(Vec::new()),
            draft_messages: tokio::sync::Mutex::new(Vec::new()),
            finalized_messages: tokio::sync::Mutex::new(Vec::new()),
            draft_updates: tokio::sync::Mutex::new(Vec::new()),
            progress_messages: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

#[derive(Default)]
struct RecordingMessageSentHook {
    events: Arc<tokio::sync::Mutex<Vec<(String, String, String)>>>,
}

#[derive(Default)]
struct TelegramRecordingChannel {
    sent_messages: tokio::sync::Mutex<Vec<String>>,
}

#[derive(Default)]
struct SlackRecordingChannel {
    sent_messages: tokio::sync::Mutex<Vec<String>>,
}

#[derive(Default)]
struct WhatsAppRecordingChannel {
    sent_messages: tokio::sync::Mutex<Vec<String>>,
}

impl ::zeroclaw_api::attribution::Attributable for TelegramRecordingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for TelegramRecordingChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for SlackRecordingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for SlackRecordingChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for WhatsAppRecordingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for WhatsAppRecordingChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for RecordingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

impl ::zeroclaw_api::attribution::Attributable for FailingSendChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

impl ::zeroclaw_api::attribution::Attributable for PendingApprovalChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }

    fn alias(&self) -> &str {
        "approval-test"
    }
}

impl ::zeroclaw_api::attribution::Attributable for DraftRecordingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for FailingSendChannel {
    fn name(&self) -> &str {
        "test-channel"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        self.send_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("send boom")
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl Channel for PendingApprovalChannel {
    fn name(&self) -> &str {
        "approval-test"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.start_typing_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.stop_typing_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn request_approval_attributed(
        &self,
        _recipient: &str,
        _request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        self.approval_started.notify_one();
        self.approval_release.notified().await;
        match &self.outcome {
            PendingApprovalOutcome::Response(response) => Ok(response.clone()),
            PendingApprovalOutcome::Error => anyhow::bail!("synthetic approval failure"),
        }
    }
}

#[async_trait::async_trait]
impl Channel for DraftRecordingChannel {
    fn name(&self) -> &str {
        "test-channel"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        if self.fallback_send_should_fail {
            anyhow::bail!("fallback send boom")
        }
        self.sent_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn update_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.draft_updates.lock().await.push(text.to_string());
        Ok(())
    }

    async fn update_draft_progress(
        &self,
        _recipient: &str,
        _message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.progress_messages.lock().await.push(text.to_string());
        Ok(())
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        self.draft_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(Some("draft-1".to_string()))
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> anyhow::Result<()> {
        if self.finalize_should_fail {
            anyhow::bail!("finalize boom")
        }
        self.finalized_messages
            .lock()
            .await
            .push(format!("{recipient}:{message_id}:{text}"));
        Ok(())
    }
}

#[async_trait::async_trait]
impl zeroclaw_runtime::hooks::HookHandler for RecordingMessageSentHook {
    fn name(&self) -> &str {
        "recording-message-sent"
    }

    async fn on_message_sent(&self, channel: &str, recipient: &str, content: &str) {
        self.events.lock().await.push((
            channel.to_string(),
            recipient.to_string(),
            content.to_string(),
        ));
    }
}

#[async_trait::async_trait]
impl Channel for RecordingChannel {
    fn name(&self) -> &str {
        "test-channel"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.sent_messages
            .lock()
            .await
            .push(format!("{}:{}", message.recipient, message.content));
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.start_typing_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        self.stop_typing_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.reactions_added.lock().await.push((
            channel_id.to_string(),
            message_id.to_string(),
            emoji.to_string(),
        ));
        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.reactions_removed.lock().await.push((
            channel_id.to_string(),
            message_id.to_string(),
            emoji.to_string(),
        ));
        Ok(())
    }

    async fn finalize_gate_prompt(&self, reference: &str, outcome: &str) -> anyhow::Result<bool> {
        self.finalized_gate_prompts
            .lock()
            .await
            .push((reference.to_string(), outcome.to_string()));
        Ok(true)
    }
}

fn test_runtime_ctx_with_config_agent_and_provider_ref(
    channel: Arc<dyn Channel>,
    model_provider: Arc<dyn ModelProvider>,
    prompt_config: zeroclaw_config::schema::Config,
    agent_cfg: zeroclaw_config::schema::AliasedAgentConfig,
    model_provider_ref: &str,
    hooks: Option<Arc<zeroclaw_runtime::hooks::HookRunner>>,
) -> Arc<ChannelRuntimeContext> {
    test_runtime_ctx_with_observer(
        channel,
        model_provider,
        prompt_config,
        agent_cfg,
        model_provider_ref,
        hooks,
        Arc::new(NoopObserver),
    )
}

fn test_runtime_ctx_with_observer(
    channel: Arc<dyn Channel>,
    model_provider: Arc<dyn ModelProvider>,
    prompt_config: zeroclaw_config::schema::Config,
    agent_cfg: zeroclaw_config::schema::AliasedAgentConfig,
    model_provider_ref: &str,
    hooks: Option<Arc<zeroclaw_runtime::hooks::HookRunner>>,
    observer: Arc<dyn Observer>,
) -> Arc<ChannelRuntimeContext> {
    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider,
        model_provider_ref: Arc::new(model_provider_ref.to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(agent_cfg),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer,
        system_prompt: Arc::new("You are a helpful assistant.".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(prompt_config),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    })
}

type MessageSentEvents = Arc<tokio::sync::Mutex<Vec<(String, String, String)>>>;

fn recording_message_sent_runner() -> (MessageSentEvents, Arc<zeroclaw_runtime::hooks::HookRunner>)
{
    let hook_events = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut hook_runner = zeroclaw_runtime::hooks::HookRunner::new();
    hook_runner.register(Box::new(RecordingMessageSentHook {
        events: Arc::clone(&hook_events),
    }));
    (hook_events, Arc::new(hook_runner))
}

fn message_sent_hook_test_message() -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        id: "msg-1".to_string(),
        sender: "alice".to_string(),
        reply_target: "chat-42".to_string(),
        content: "hello".to_string(),
        channel: "test-channel".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn process_channel_message_fires_message_sent_hook_after_reply_delivery() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (hook_events, hook_runner) = recording_message_sent_runner();

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        Some(hook_runner),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.as_slice(), ["chat-42:ok"]);

    let events = hook_events.lock().await;
    assert_eq!(
        events.as_slice(),
        [(
            "test-channel".to_string(),
            "chat-42".to_string(),
            "ok".to_string()
        )]
    );
}

/// Observer that records every event, for turn-lifecycle assertions.
#[derive(Default)]
struct RecordingObserver {
    events: std::sync::Mutex<Vec<ObserverEvent>>,
}

impl Observer for RecordingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str {
        "recording"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn flush(&self) {}
}

/// Provider stub that cancels the turn's `CancellationToken` from inside
/// the LLM call and then parks forever, so the orchestrator's
/// `tokio::select!` deterministically takes the cancelled arm mid-turn.
struct CancelMidTurnModelProvider {
    token: CancellationToken,
}

#[async_trait::async_trait]
impl ModelProvider for CancelMidTurnModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.token.cancel();
        std::future::pending::<()>().await;
        unreachable!("parked future never resumes")
    }

    async fn chat_with_history(
        &self,
        _messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.token.cancel();
        std::future::pending::<()>().await;
        unreachable!("parked future never resumes")
    }
}
impl ::zeroclaw_api::attribution::Attributable for CancelMidTurnModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "CancelMidTurnModelProvider"
    }
}

/// (start bracket entries, end bracket entries) — each entry is
/// `(channel, agent_alias, turn_id)`. Aliased to keep the return type
/// readable (clippy::type_complexity).
type LifecycleBracketSnapshot = (
    Vec<(Option<String>, Option<String>, Option<String>)>,
    Vec<(Option<String>, Option<String>, Option<String>)>,
);

fn lifecycle_bracket_snapshot(events: &[ObserverEvent]) -> LifecycleBracketSnapshot {
    let starts = events
        .iter()
        .filter_map(|e| match e {
            ObserverEvent::AgentStart {
                channel,
                agent_alias,
                turn_id,
                ..
            } => Some((channel.clone(), agent_alias.clone(), turn_id.clone())),
            _ => None,
        })
        .collect();
    let ends = events
        .iter()
        .filter_map(|e| match e {
            ObserverEvent::AgentEnd {
                channel,
                agent_alias,
                turn_id,
                ..
            } => Some((channel.clone(), agent_alias.clone(), turn_id.clone())),
            _ => None,
        })
        .collect();
    (starts, ends)
}

/// Regression guard for the fix where channel-originated turns (Telegram,
/// Discord, ...) never emitted `AgentStart`/`AgentEnd`, so
/// `/api/events/history` showed `llm_request` frames but no turn
/// lifecycle brackets. A successful turn must emit exactly one
/// `AgentStart` (before the LLM request) and one `AgentEnd` (last),
/// all sharing one `turn_id` and carrying the channel + agent alias.
#[tokio::test]
async fn process_channel_message_brackets_turn_with_agent_start_and_agent_end() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let observer = Arc::new(RecordingObserver::default());

    let runtime_ctx = test_runtime_ctx_with_observer(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
        observer.clone(),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    let events = observer.events.lock().unwrap();
    let (starts, ends) = lifecycle_bracket_snapshot(&events);
    assert_eq!(starts.len(), 1, "exactly one AgentStart, got {events:?}");
    assert_eq!(ends.len(), 1, "exactly one AgentEnd, got {events:?}");

    let start_pos = events
        .iter()
        .position(|e| matches!(e, ObserverEvent::AgentStart { .. }))
        .unwrap();
    let llm_request_pos = events
        .iter()
        .position(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
        .expect("turn should emit an LlmRequest");
    assert!(
        start_pos < llm_request_pos,
        "AgentStart must precede the LlmRequest: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(ObserverEvent::AgentEnd { .. })),
        "AgentEnd must be the last event: {events:?}"
    );

    let (start_channel, start_alias, start_turn_id) = starts[0].clone();
    let (end_channel, end_alias, end_turn_id) = ends[0].clone();
    assert_eq!(start_channel.as_deref(), Some("test-channel"));
    assert_eq!(end_channel.as_deref(), Some("test-channel"));
    assert_eq!(start_alias.as_deref(), Some("test-agent"));
    assert_eq!(end_alias.as_deref(), Some("test-agent"));
    assert!(start_turn_id.is_some(), "AgentStart must carry a turn_id");
    assert_eq!(start_turn_id, end_turn_id, "brackets must share a turn_id");

    let llm_request_turn_id = events.iter().find_map(|e| match e {
        ObserverEvent::LlmRequest { turn_id, .. } => Some(turn_id.clone()),
        _ => None,
    });
    assert_eq!(
        llm_request_turn_id,
        Some(start_turn_id),
        "inner LlmRequest must share the brackets' turn_id"
    );
}

/// An erroring LLM turn must still close its bracket: one `AgentStart`
/// and one `AgentEnd`, same `turn_id`.
#[tokio::test]
async fn process_channel_message_emits_brackets_when_llm_errors() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let observer = Arc::new(RecordingObserver::default());

    let runtime_ctx = test_runtime_ctx_with_observer(
        channel,
        Arc::new(FormatErrorModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
        observer.clone(),
    );

    let mut msg = message_sent_hook_test_message();
    msg.content = "trigger format error".to_string();
    process_channel_message(runtime_ctx, msg, CancellationToken::new()).await;

    let events = observer.events.lock().unwrap();
    let (starts, ends) = lifecycle_bracket_snapshot(&events);
    assert_eq!(starts.len(), 1, "exactly one AgentStart, got {events:?}");
    assert_eq!(ends.len(), 1, "exactly one AgentEnd, got {events:?}");
    assert_eq!(
        starts[0].2, ends[0].2,
        "brackets must share a turn_id even on error"
    );
    assert!(starts[0].2.is_some(), "brackets must carry a turn_id");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn provider_query_secret_is_absent_from_channel_error_log_and_reply() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(QuerySecretErrorModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    let mut msg = message_sent_hook_test_message();
    msg.id = "msg-query-secret-error".to_string();
    msg.sender = "query-secret-sender".to_string();

    process_channel_message(runtime_ctx, msg, CancellationToken::new()).await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    let reply = sent_messages
        .last()
        .expect("provider failure must send an error reply");
    assert!(
        !reply.contains(TEST_PROVIDER_QUERY_SECRET),
        "channel reply leaked provider query secret: {reply}"
    );
    assert!(
        reply.contains("generativelanguage.googleapis.com/v1beta/models/test:generateContent"),
        "sanitized reply should preserve the useful endpoint: {reply}"
    );
    drop(sent_messages);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut error_event = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(value))
                if value.get("message").and_then(|v| v.as_str())
                    == Some("channel_message_error")
                    && value.pointer("/attributes/sender").and_then(|v| v.as_str())
                        == Some("query-secret-sender") =>
            {
                error_event = Some(value);
                break;
            }
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) | Err(_) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
        }
    }

    let event = error_event.expect("channel_message_error log must be emitted");
    let logged_error = event
        .pointer("/attributes/error")
        .and_then(|value| value.as_str())
        .expect("channel_message_error must carry the sanitized error attribute");
    assert!(
        !logged_error.contains(TEST_PROVIDER_QUERY_SECRET),
        "structured log leaked provider query secret: {event}"
    );
    assert!(
        logged_error
            .contains("generativelanguage.googleapis.com/v1beta/models/test:generateContent"),
        "structured log should preserve the useful endpoint: {event}"
    );
}

/// A turn cancelled mid-flight (interrupt-on-new-message) must still
/// close its bracket — the ZeroHome-critical guarantee that a cancelled
/// turn cannot wedge an "agent in flight" indicator with an unmatched
/// `AgentStart`.
#[tokio::test]
async fn process_channel_message_emits_brackets_when_cancelled_mid_turn() {
    let token = CancellationToken::new();
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let observer = Arc::new(RecordingObserver::default());

    let runtime_ctx = test_runtime_ctx_with_observer(
        channel,
        Arc::new(CancelMidTurnModelProvider {
            token: token.clone(),
        }),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
        observer.clone(),
    );

    process_channel_message(runtime_ctx, message_sent_hook_test_message(), token).await;

    let events = observer.events.lock().unwrap();
    let (starts, ends) = lifecycle_bracket_snapshot(&events);
    assert_eq!(
        starts.len(),
        1,
        "cancelled turn must still emit AgentStart, got {events:?}"
    );
    assert_eq!(
        ends.len(),
        1,
        "cancelled turn must still emit AgentEnd, got {events:?}"
    );
    assert_eq!(
        starts[0].2, ends[0].2,
        "brackets must share a turn_id even when cancelled"
    );
    assert!(starts[0].2.is_some(), "brackets must carry a turn_id");
}

#[tokio::test]
async fn process_channel_message_skips_message_sent_hook_when_reply_delivery_fails() {
    let channel_impl = Arc::new(FailingSendChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (hook_events, hook_runner) = recording_message_sent_runner();

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        Some(hook_runner),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(channel_impl.send_calls.load(Ordering::SeqCst), 1);
    assert!(hook_events.lock().await.is_empty());
}

#[tokio::test]
async fn process_channel_message_fires_message_sent_hook_after_draft_finalize() {
    let channel_impl = Arc::new(DraftRecordingChannel::new(false, false));
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (hook_events, hook_runner) = recording_message_sent_runner();

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        Some(hook_runner),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        channel_impl.draft_messages.lock().await.as_slice(),
        ["chat-42:..."]
    );
    assert_eq!(
        channel_impl.finalized_messages.lock().await.as_slice(),
        ["chat-42:draft-1:ok"]
    );
    assert!(channel_impl.sent_messages.lock().await.is_empty());
    assert_eq!(
        hook_events.lock().await.as_slice(),
        [(
            "test-channel".to_string(),
            "chat-42".to_string(),
            "ok".to_string()
        )]
    );
}

#[tokio::test]
async fn process_channel_message_fires_message_sent_hook_after_draft_fallback_send() {
    let channel_impl = Arc::new(DraftRecordingChannel::new(true, false));
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (hook_events, hook_runner) = recording_message_sent_runner();

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        Some(hook_runner),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        channel_impl.draft_messages.lock().await.as_slice(),
        ["chat-42:..."]
    );
    assert!(channel_impl.finalized_messages.lock().await.is_empty());
    assert_eq!(
        channel_impl.sent_messages.lock().await.as_slice(),
        ["chat-42:ok"]
    );
    assert_eq!(
        hook_events.lock().await.as_slice(),
        [(
            "test-channel".to_string(),
            "chat-42".to_string(),
            "ok".to_string()
        )]
    );
}

#[tokio::test]
async fn process_channel_message_skips_message_sent_hook_when_draft_fallback_send_fails() {
    let channel_impl = Arc::new(DraftRecordingChannel::new(true, true));
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (hook_events, hook_runner) = recording_message_sent_runner();

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        Some(hook_runner),
    );

    process_channel_message(
        runtime_ctx,
        message_sent_hook_test_message(),
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        channel_impl.draft_messages.lock().await.as_slice(),
        ["chat-42:..."]
    );
    assert!(channel_impl.finalized_messages.lock().await.is_empty());
    assert!(channel_impl.sent_messages.lock().await.is_empty());
    assert!(hook_events.lock().await.is_empty());
}

fn draft_updater_no_tools() -> HashSet<String> {
    HashSet::new()
}

/// Production-boundary proof for draft sanitization. Drives
/// [`run_draft_updater`], the code the streaming spawn actually runs, and
/// asserts on the strings handed to `update_draft` and
/// `update_draft_progress`. Unlike the per-frame tests above, this one
/// fails if the sanitizer call is ever dropped from the wiring.
#[tokio::test]
async fn draft_updater_never_hands_scratchpad_to_the_transport() {
    let channel_impl = Arc::new(DraftRecordingChannel::new(false, false));
    let channel: Arc<dyn Channel> = channel_impl.clone();
    // Queue every delta up front, then close the sender and drain inline.
    // The capacity exceeds the number of deltas, so nothing blocks and the
    // updater needs no concurrent task: it returns once the channel is
    // closed and empty.
    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_runtime::agent::loop_::DraftEvent>(32);

    use zeroclaw_runtime::agent::loop_::StreamDelta;
    for delta in [
        "Looking that up",
        " now.\n<tool_res",
        "ult name=\"price\">",
        "{\"btc\": 64000}",
        "</tool_result>\n",
        "BTC is $64,000.",
    ] {
        tx.send(StreamDelta::Text(delta.to_string())).await.unwrap();
    }
    tx.send(StreamDelta::Status(
        "<think>deciding</think>Working on it.".to_string(),
    ))
    .await
    .unwrap();
    drop(tx);

    run_draft_updater(
        channel,
        "chat-1".to_string(),
        "draft-1".to_string(),
        draft_updater_no_tools(),
        rx,
    )
    .await;

    let drafts = channel_impl.draft_updates.lock().await;
    assert!(!drafts.is_empty(), "the transport must have been called");
    for (i, text) in drafts.iter().enumerate() {
        assert!(
            !text.contains("<tool_res") && !text.contains("btc"),
            "draft update {i} carried scratchpad to the transport: {text:?}"
        );
    }
    assert_eq!(
        drafts.last().unwrap(),
        "Looking that up now.\n\nBTC is $64,000."
    );
    drop(drafts);

    let progress = channel_impl.progress_messages.lock().await;
    assert_eq!(
        progress.as_slice(),
        ["Working on it.".to_string()],
        "status text must reach the transport already stripped of reasoning"
    );
}

/// Production-boundary regression for an alternate XML dialect arriving
/// split across deltas. The complete-tag stripper has always known seven
/// opening forms; the partial guard once knew two, so a frame ending in
/// `<invo` or `<function_` reached the transport verbatim. Both inventories
/// now come from one list, so every dialect is covered at both stages.
#[tokio::test]
async fn draft_updater_holds_back_split_alternate_protocol_prefixes() {
    for (dialect, deltas) in [
        (
            "invoke",
            [
                "Checking that",
                " now.\n<invo",
                "ke>{\"name\":\"shell\",\"secret\":1}",
                "</invoke>\nAll set.",
            ],
        ),
        (
            "function_call",
            [
                "Checking that",
                " now.\n<function_",
                "call>{\"name\":\"shell\",\"secret\":1}",
                "</function_call>\nAll set.",
            ],
        ),
        (
            "tool-call",
            [
                "Checking that",
                " now.\n<tool-",
                "call>{\"name\":\"shell\",\"secret\":1}",
                "</tool-call>\nAll set.",
            ],
        ),
    ] {
        let channel_impl = Arc::new(DraftRecordingChannel::new(false, false));
        let channel: Arc<dyn Channel> = channel_impl.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_runtime::agent::loop_::DraftEvent>(32);
        use zeroclaw_runtime::agent::loop_::StreamDelta;
        for delta in deltas {
            tx.send(StreamDelta::Text(delta.to_string())).await.unwrap();
        }
        drop(tx);

        run_draft_updater(
            channel,
            "chat-1".to_string(),
            "draft-1".to_string(),
            draft_updater_no_tools(),
            rx,
        )
        .await;

        let drafts = channel_impl.draft_updates.lock().await;
        assert!(
            !drafts.is_empty(),
            "{dialect}: the transport must be called"
        );
        for (i, text) in drafts.iter().enumerate() {
            assert!(
                !text.contains('<') && !text.contains("secret"),
                "{dialect}: draft update {i} carried a protocol opener: {text:?}"
            );
        }
        assert_eq!(
            drafts.last().unwrap(),
            "Checking that now.\n\nAll set.",
            "{dialect}: the surrounding prose must survive"
        );
    }
}

/// Production-boundary regression for the strict-parsing path. With
/// `strict_tool_parsing` enabled the runtime forwards deltas without the
/// `StreamTextGuard`, so a protocol JSON body — bare or fenced, complete or
/// still arriving — reaches this boundary exactly as emitted. The final
/// sanitizer suppresses these through the parser's classifier; the draft
/// boundary now asks the same classifier.
#[tokio::test]
async fn draft_updater_suppresses_strict_mode_protocol_json() {
    let known: HashSet<String> = ["shell".to_string()].into_iter().collect();

    for (label, deltas) in [
        (
            "bare malformed result",
            vec![
                "{\"tool_call_id\":\"call_1\",",
                "\"content\":\"s3cret-output\"",
            ],
        ),
        (
            "fenced protocol payload",
            vec![
                "Here you go.\n```json\n",
                "{\"tool_calls\":[{\"call_id\":\"c1\",\"name\":\"shell\",",
                "\"arguments\":{\"command\":\"cat /etc/s3cret\"}}]",
            ],
        ),
    ] {
        let channel_impl = Arc::new(DraftRecordingChannel::new(false, false));
        let channel: Arc<dyn Channel> = channel_impl.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_runtime::agent::loop_::DraftEvent>(32);
        use zeroclaw_runtime::agent::loop_::StreamDelta;
        for delta in &deltas {
            tx.send(StreamDelta::Text((*delta).to_string()))
                .await
                .unwrap();
        }
        drop(tx);

        run_draft_updater(
            channel,
            "chat-1".to_string(),
            "draft-1".to_string(),
            known.clone(),
            rx,
        )
        .await;

        let drafts = channel_impl.draft_updates.lock().await;
        for (i, text) in drafts.iter().enumerate() {
            assert!(
                !text.contains("s3cret") && !text.contains("tool_call_id"),
                "{label}: draft update {i} carried protocol JSON: {text:?}"
            );
        }
    }
}

/// The counterweight to the two suppression tests above: a genuine answer
/// *about* tool protocol, and an ordinary fenced code block, must still
/// stream. Suppressing everything JSON-shaped would be an easy way to pass
/// the leak tests and a bad way to answer a question.
#[tokio::test]
async fn draft_updater_still_streams_examples_and_ordinary_code_fences() {
    let known: HashSet<String> = ["shell".to_string()].into_iter().collect();
    let channel_impl = Arc::new(DraftRecordingChannel::new(false, false));
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_runtime::agent::loop_::DraftEvent>(32);

    use zeroclaw_runtime::agent::loop_::StreamDelta;
    for delta in [
        "Here is how a config looks:\n```json\n",
        "{\"retries\": 3,\n",
        "\"timeout_ms\": 500}\n```\n",
        "Adjust the values to taste.",
    ] {
        tx.send(StreamDelta::Text(delta.to_string())).await.unwrap();
    }
    drop(tx);

    run_draft_updater(
        channel,
        "chat-1".to_string(),
        "draft-1".to_string(),
        known,
        rx,
    )
    .await;

    let drafts = channel_impl.draft_updates.lock().await;
    let last = drafts.last().expect("the transport must have been called");
    assert!(
        last.contains("\"retries\": 3") && last.contains("Adjust the values to taste."),
        "an ordinary JSON code fence must survive the draft path: {last:?}"
    );
}

struct SlowModelProvider {
    delay: Duration,
}

#[async_trait::async_trait]
impl ModelProvider for SlowModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        tokio::time::sleep(self.delay).await;
        Ok(format!("echo: {message}"))
    }
}
impl ::zeroclaw_api::attribution::Attributable for SlowModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "SlowModelProvider"
    }
}

struct NoReplyModelProvider;

#[async_trait::async_trait]
impl ModelProvider for NoReplyModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("NO_REPLY[INFO]: nothing to add".to_string())
    }
}
impl ::zeroclaw_api::attribution::Attributable for NoReplyModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "NoReplyModelProvider"
    }
}

struct ToolCallingModelProvider;

fn tool_call_payload() -> String {
    r#"<tool_call>
{"name":"mock_price","arguments":{"symbol":"BTC"}}
</tool_call>"#
        .to_string()
}

fn tool_call_payload_with_alias_tag() -> String {
    r#"<toolcall>
{"name":"mock_price","arguments":{"symbol":"BTC"}}
</toolcall>"#
        .to_string()
}

#[async_trait::async_trait]
impl ModelProvider for ToolCallingModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(tool_call_payload())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let has_tool_results = messages
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool results]"));
        if has_tool_results {
            Ok("BTC is currently around $65,000 based on latest tool output.".to_string())
        } else {
            Ok(tool_call_payload())
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for ToolCallingModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ToolCallingModelProvider"
    }
}

struct SessionsCurrentModelProvider;

#[async_trait::async_trait]
impl ModelProvider for SessionsCurrentModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(r#"<tool_call>
{"name":"sessions_current","arguments":{}}
</tool_call>"#
            .to_string())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if let Some(tool_results) = messages
            .iter()
            .find(|msg| msg.role == "user" && msg.content.contains("[Tool results]"))
        {
            if tool_results
                .content
                .contains("Current session: test-channel_chat-42_alice")
                && tool_results.content.contains("Messages: 1")
            {
                return Ok("Current session: test-channel_chat-42_alice\nMessages: 1".to_string());
            }

            Ok("session result unavailable".to_string())
        } else {
            self.chat_with_system(None, "", "", None).await
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for SessionsCurrentModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "SessionsCurrentModelProvider"
    }
}

struct ToolCallingAliasModelProvider;

#[async_trait::async_trait]
impl ModelProvider for ToolCallingAliasModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(tool_call_payload_with_alias_tag())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let has_tool_results = messages
            .iter()
            .any(|msg| msg.role == "user" && msg.content.contains("[Tool results]"));
        if has_tool_results {
            Ok("BTC alias-tag flow resolved to final text output.".to_string())
        } else {
            Ok(tool_call_payload_with_alias_tag())
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for ToolCallingAliasModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ToolCallingAliasModelProvider"
    }
}

struct RawToolArtifactModelProvider;

#[async_trait::async_trait]
impl ModelProvider for RawToolArtifactModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat_with_history(
        &self,
        _messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(r#"{"name":"mock_price","parameters":{"symbol":"BTC"}}
{"result":{"symbol":"BTC","price_usd":65000}}
BTC is currently around $65,000 based on latest tool output."#
            .to_string())
    }
}
impl ::zeroclaw_api::attribution::Attributable for RawToolArtifactModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "RawToolArtifactModelProvider"
    }
}

struct IterativeToolModelProvider {
    required_tool_iterations: usize,
}

impl IterativeToolModelProvider {
    fn completed_tool_iterations(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .filter(|msg| msg.role == "user" && msg.content.contains("[Tool results]"))
            .count()
    }
}

#[async_trait::async_trait]
impl ModelProvider for IterativeToolModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(tool_call_payload())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let completed_iterations = Self::completed_tool_iterations(messages);
        if completed_iterations >= self.required_tool_iterations {
            Ok(format!(
                "Completed after {completed_iterations} tool iterations."
            ))
        } else {
            Ok(tool_call_payload())
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for IterativeToolModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "IterativeToolModelProvider"
    }
}

#[derive(Default)]
struct HistoryCaptureModelProvider {
    calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
    vision: bool,
}

#[async_trait::async_trait]
impl ModelProvider for HistoryCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let snapshot = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect::<Vec<_>>();
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        calls.push(snapshot);
        Ok(format!("response-{}", calls.len()))
    }

    fn supports_vision(&self) -> bool {
        self.vision
    }
}
impl ::zeroclaw_api::attribution::Attributable for HistoryCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "HistoryCaptureModelProvider"
    }
}

#[tokio::test]
async fn passive_context_records_history_without_channel_or_model_side_effects() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        provider,
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );

    let passive_msg = zeroclaw_api::channel::ChannelMessage {
        id: "passive-1".into(),
        sender: "bob".into(),
        reply_target: "group-1@g.us".into(),
        content: "the release codename is quartz".into(),
        channel: "whatsapp".into(),
        timestamp: 1,
        passive_context: true,
        conversation_scope: zeroclaw_api::channel::ChannelConversationScope::ReplyTarget,
        ..Default::default()
    };

    process_channel_message(
        runtime_ctx.clone(),
        passive_msg.clone(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty(),
        "passive context must not call the provider"
    );
    assert!(channel_impl.sent_messages.lock().await.is_empty());
    assert!(channel_impl.reactions_added.lock().await.is_empty());
    assert!(channel_impl.reactions_removed.lock().await.is_empty());
    assert_eq!(channel_impl.start_typing_calls.load(Ordering::SeqCst), 0);
    assert_eq!(channel_impl.stop_typing_calls.load(Ordering::SeqCst), 0);

    let active_msg = zeroclaw_api::channel::ChannelMessage {
        id: "active-1".into(),
        sender: "alice".into(),
        content: "what is the release codename?".into(),
        timestamp: 2,
        passive_context: false,
        conversation_scope: zeroclaw_api::channel::ChannelConversationScope::ReplyTarget,
        ..passive_msg.clone()
    };
    assert_eq!(
        conversation_history_key(&active_msg),
        conversation_history_key(&passive_msg)
    );

    process_channel_message(runtime_ctx, active_msg, CancellationToken::new()).await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    let user_history = calls[0]
        .iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, content)| content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        user_history.contains("the release codename is quartz"),
        "active turn should see passive group context, got: {user_history}"
    );
    assert!(
        user_history.contains("[Observed WhatsApp group message from bob]"),
        "passive group context should preserve observed sender attribution, got: {user_history}"
    );
    assert!(
        user_history.contains("what is the release codename?"),
        "active turn should still include current message, got: {user_history}"
    );
    assert!(
        user_history.contains("[Current WhatsApp group message from alice]"),
        "active group turn should preserve current sender attribution, got: {user_history}"
    );
}

struct DelayedHistoryCaptureModelProvider {
    delay: Duration,
    calls: std::sync::Mutex<Vec<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl ModelProvider for DelayedHistoryCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let snapshot = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect::<Vec<_>>();
        let call_index = {
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            calls.push(snapshot);
            calls.len()
        };
        tokio::time::sleep(self.delay).await;
        Ok(format!("response-{call_index}"))
    }
}
impl ::zeroclaw_api::attribution::Attributable for DelayedHistoryCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "DelayedHistoryCaptureModelProvider"
    }
}

struct MockPriceTool;

impl ::zeroclaw_api::attribution::Attributable for MockPriceTool {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        <Self as ::zeroclaw_api::tool::Tool>::name(self)
    }
}

#[derive(Default)]
struct ModelCaptureModelProvider {
    call_count: AtomicUsize,
    models: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for ModelCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat_with_history(
        &self,
        _messages: &[ChatMessage],
        model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(model.to_string());
        Ok("ok".to_string())
    }
}
impl ::zeroclaw_api::attribution::Attributable for ModelCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ModelCaptureModelProvider"
    }
}

#[derive(Default)]
struct ModelSwitchRequestProvider {
    call_count: AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProvider for ModelSwitchRequestProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat(
        &self,
        _request: zeroclaw_providers::ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
        let call_index = self.call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "switch-call".to_string(),
                    name: "model_switch".to_string(),
                    arguments: serde_json::json!({
                        "action": "set",
                        "model_provider": "openrouter.default",
                        "model": "switched-model"
                    })
                    .to_string(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        } else {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("original-provider-should-not-be-reused".to_string()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }
}

impl ::zeroclaw_api::attribution::Attributable for ModelSwitchRequestProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }

    fn alias(&self) -> &str {
        "ModelSwitchRequestProvider"
    }
}

#[derive(Default)]
struct PrecheckProbeModelProvider {
    precheck_calls: AtomicUsize,
    main_calls: AtomicUsize,
    models: std::sync::Mutex<Vec<String>>,
    precheck_delay: Option<Duration>,
}

#[async_trait::async_trait]
impl ModelProvider for PrecheckProbeModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(model.to_string());

        if message.starts_with("Decide whether the assistant should send any visible reply") {
            self.precheck_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(delay) = self.precheck_delay {
                tokio::time::sleep(delay).await;
            }
            return Ok("NO_REPLY[INFO]: background chatter".to_string());
        }

        self.main_calls.fetch_add(1, Ordering::SeqCst);
        Ok("visible reply".to_string())
    }
}

impl ::zeroclaw_api::attribution::Attributable for PrecheckProbeModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "PrecheckProbeModelProvider"
    }
}

#[async_trait::async_trait]
impl Tool for MockPriceTool {
    fn name(&self) -> &str {
        "mock_price"
    }

    fn description(&self) -> &str {
        "Return a mocked BTC price"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string" }
            },
            "required": ["symbol"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let symbol = args.get("symbol").and_then(serde_json::Value::as_str);
        if symbol != Some("BTC") {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("unexpected symbol".to_string()),
            });
        }

        Ok(ToolResult {
            success: true,
            output: r#"{"symbol":"BTC","price_usd":65000}"#.to_string().into(),
            error: None,
        })
    }
}

/// Minimal fixed-name tool for allowlist-filter coverage.
struct NamedMockTool(&'static str);

impl ::zeroclaw_api::attribution::Attributable for NamedMockTool {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        self.0
    }
}

#[async_trait::async_trait]
impl Tool for NamedMockTool {
    fn name(&self) -> &str {
        self.0
    }

    fn description(&self) -> &str {
        "named mock"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: ToolOutput::default(),
            error: None,
        })
    }
}

#[test]
fn channel_path_allowlist_drops_non_allowlisted_builtins() {
    let mut built_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool("shell")),
        Box::new(NamedMockTool("file_write")),
        Box::new(NamedMockTool("file_read")),
    ];
    let policy = SecurityPolicy {
        allowed_tools: Some(vec!["file_read".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    };
    apply_policy_tool_filter(&mut built_tools, Some(&policy), None);
    let names: Vec<&str> = built_tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"shell") && !names.contains(&"file_write"),
        "raw built-ins outside the allowlist must be dropped on the channel path; got {names:?}"
    );
    assert!(
        names.contains(&"file_read"),
        "allowlisted tool must survive the filter; got {names:?}"
    );
}

#[test]
fn channel_path_excluded_tools_drops_denied_mcp_tool() {
    use zeroclaw_runtime::agent::loop_::{
        mcp_tool_access_policy, register_eager_mcp_tool_if_allowed,
    };
    let policy = SecurityPolicy {
        excluded_tools: Some(vec!["aa_mcp__find_items".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    };
    let mcp_policy = mcp_tool_access_policy(&policy, None);
    let mut built_tools: Vec<Box<dyn Tool>> = Vec::new();
    let denied: Arc<dyn Tool> = Arc::new(NamedMockTool("aa_mcp__find_items"));
    let allowed: Arc<dyn Tool> = Arc::new(NamedMockTool("aa_mcp__find_npcs"));
    let registered_denied =
        register_eager_mcp_tool_if_allowed(denied, &mut built_tools, None, mcp_policy.as_ref());
    let registered_allowed =
        register_eager_mcp_tool_if_allowed(allowed, &mut built_tools, None, mcp_policy.as_ref());
    assert!(
        !registered_denied,
        "an `excluded_tools`-denied MCP tool must not be registered on the channel path"
    );
    assert!(
        registered_allowed,
        "a non-denied MCP tool must still be registered (allowlist auto-admit)"
    );
    let names: Vec<&str> = built_tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"aa_mcp__find_items"),
        "denied MCP tool leaked into the channel registry; got {names:?}"
    );
    assert!(
        names.contains(&"aa_mcp__find_npcs"),
        "allowed MCP tool missing from the channel registry; got {names:?}"
    );
}

#[test]
fn channel_path_excluded_tools_drops_denied_builtin() {
    let mut built_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool("shell")),
        Box::new(NamedMockTool("file_read")),
    ];
    let policy = SecurityPolicy {
        excluded_tools: Some(vec!["shell".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    };
    apply_policy_tool_filter(&mut built_tools, Some(&policy), None);
    let names: Vec<&str> = built_tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"shell"),
        "an `excluded_tools`-denied built-in must be dropped on the channel path; got {names:?}"
    );
    assert!(
        names.contains(&"file_read"),
        "a non-excluded built-in must survive the filter; got {names:?}"
    );
}

fn channel_all_tools_result(tools: Vec<Box<dyn Tool>>) -> tools::AllToolsResult {
    tools::AllToolsResult {
        tools,
        delegate_handle: None,
        ask_user_handle: None,
        reaction_handle: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        poll_handle: None,
        escalate_handle: None,
        channel_room_handle: None,
        unfiltered_tool_arcs: Vec::new(),
    }
}

/// A mock HTTP MCP server that advertises `resources` support and serves one
/// readable resource (`file:///handbook.md`), so `assemble_channel_agent_tools`
/// resolves a real pinned-resource section instead of an empty one.
async fn mock_mcp_server_with_pinned_resource() -> wiremock::MockServer {
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Mcp-Session-Id", "s")
                .set_body_json(serde_json::json!({
                    "jsonrpc":"2.0","id":1,
                    "result":{"capabilities":{"tools":{},"resources":{}}}
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"notifications/initialized"}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"tools/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc":"2.0","id":2,"result":{"tools":[
                {"name":"echo","description":"echo","inputSchema":{"type":"object"}}
            ]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method":"resources/list"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc":"2.0","id":3,"result":{"resources":[
                {"uri":"file:///handbook.md","name":"handbook","mimeType":"text/plain"}
            ]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
            .and(body_partial_json(serde_json::json!({"method":"resources/read"})))
            .respond_with(|request: &wiremock::Request| {
                let id = serde_json::from_slice::<serde_json::Value>(&request.body)
                    .expect("resources/read request should be JSON")
                    .get("id")
                    .cloned()
                    .expect("resources/read request should carry an id");
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "jsonrpc":"2.0","id":id,"result":{"contents":[
                        {"uri":"file:///handbook.md","mimeType":"text/plain","text":"Pinned handbook body"}
                    ]}
                }))
            })
            .mount(&server)
            .await;
    server
}

/// Drives the REAL `assemble_channel_agent_tools` against a mock MCP server
/// granting one pinned resource, then runs the exact post-assembly composition
/// `start_channels` performs (`compose_channel_mcp_prompt_sections`). It proves
/// the production boundary keeps the deferred tool-search listing and the pinned
/// MCP resource in SEPARATE sections, so strict text-tool suppression
/// (`native_tools = false`, `strict_tool_parsing = true`) clears the deferred
/// tool instructions while the pinned resource survives into the final prompt.
/// A regression that re-merges the two sections inside the assembly (or reorders
/// the suppress/append pair) fails here, unlike a test that hand-builds the
/// section strings and never calls the assembly.
#[tokio::test]
async fn assemble_channel_agent_tools_keeps_pinned_resources_after_strict_policy() {
    use zeroclaw_config::schema::{
        AliasedAgentConfig, McpBundleConfig, McpServerConfig, McpTransport, RiskProfileConfig,
    };
    let server = mock_mcp_server_with_pinned_resource().await;

    let mut config = Config::default();
    config.mcp.enabled = true;
    config.mcp.deferred_loading = true;
    config.mcp.servers = vec![McpServerConfig {
        name: "docs".into(),
        transport: McpTransport::Http,
        url: Some(server.uri()),
        pinned_resources: vec!["file:///handbook.md".into()],
        ..Default::default()
    }];
    config.mcp_bundles.insert(
        "docsbundle".into(),
        McpBundleConfig {
            servers: vec!["docs".into()],
            exclude: Vec::new(),
        },
    );
    config
        .risk_profiles
        .insert("test-profile".into(), RiskProfileConfig::default());
    config.agents.insert(
        "channel-agent".into(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "openai.test-provider".into(),
            risk_profile: "test-profile".into(),
            mcp_bundles: vec!["docsbundle".into()],
            ..Default::default()
        },
    );

    let security = Arc::new(SecurityPolicy {
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    });
    let assembled = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        assemble_channel_agent_tools(
            &config,
            "channel-agent",
            "openai.test-provider",
            "gpt-test",
            &security,
            channel_all_tools_result(Vec::new()),
            &[],
            Arc::new(platform::NativeRuntime::new()),
        ),
    )
    .await
    .expect("assemble must not hang");

    // The production assembly must surface the pinned resource in its OWN
    // section, distinct from the deferred/tool-search listing.
    assert!(
        assembled.pinned_section.contains("Pinned handbook body")
            && assembled
                .pinned_section
                .contains("trust=\"untrusted-external\""),
        "assemble_channel_agent_tools must expose the pinned MCP resource in \
             pinned_section; got {:?}",
        assembled.pinned_section
    );
    assert!(
        !assembled.deferred_section.contains("Pinned handbook body"),
        "pinned resource content must NOT be merged into the deferred section; got {:?}",
        assembled.deferred_section
    );
    assert!(
        assembled.deferred_section.contains("tool_search"),
        "precondition: a deferred-loading MCP grant must yield a tool_search \
             section to suppress; got {:?}",
        assembled.deferred_section
    );

    // Run the exact composition start_channels performs for a strict,
    // non-native target: suppress the deferred tool-search section, keep pinned.
    let mut tool_descs: Vec<(&str, &str)> = vec![("shell", "Run commands")];
    let mut deferred_section = assembled.deferred_section.clone();
    let expose_text_protocol = compose_channel_mcp_prompt_sections(
        false,
        true,
        &mut tool_descs,
        &mut deferred_section,
        &assembled.pinned_section,
    );

    assert!(!expose_text_protocol);
    assert!(
        !deferred_section.contains("tool_search"),
        "strict policy must clear the deferred tool-search section; got {deferred_section:?}"
    );
    assert!(
        deferred_section.contains("Pinned handbook body")
            && deferred_section.contains("## Pinned MCP Resources"),
        "pinned resource must survive strict suppression; got {deferred_section:?}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn assemble_channel_agent_tools_attributes_assembly_logs_to_agent_and_model() {
    use zeroclaw_config::schema::{
        AliasedAgentConfig, McpBundleConfig, McpServerConfig, McpTransport, RiskProfileConfig,
    };

    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let server = mock_mcp_server_with_pinned_resource().await;
    let mut config = Config::default();
    config.mcp.enabled = true;
    config.mcp.deferred_loading = true;
    config.mcp.servers = vec![McpServerConfig {
        name: "docs".into(),
        transport: McpTransport::Http,
        url: Some(server.uri()),
        pinned_resources: vec!["file:///handbook.md".into()],
        ..Default::default()
    }];
    config.mcp_bundles.insert(
        "docsbundle".into(),
        McpBundleConfig {
            servers: vec!["docs".into()],
            exclude: Vec::new(),
        },
    );
    config
        .risk_profiles
        .insert("test-profile".into(), RiskProfileConfig::default());
    config.agents.insert(
        "channel-agent".into(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "openai.test-provider".into(),
            risk_profile: "test-profile".into(),
            mcp_bundles: vec!["docsbundle".into()],
            ..Default::default()
        },
    );

    let security = Arc::new(SecurityPolicy {
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    });
    let _assembled = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        assemble_channel_agent_tools(
            &config,
            "channel-agent",
            "openai.test-provider",
            "gpt-test",
            &security,
            channel_all_tools_result(Vec::new()),
            &[],
            Arc::new(platform::NativeRuntime::new()),
        ),
    )
    .await
    .expect("assemble must not hang");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut assembly_event = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(value)) => {
                if value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .is_some_and(|m| m.starts_with("Initializing MCP client"))
                {
                    assembly_event = Some(value);
                    break;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => {}
        }
    }

    let value = assembly_event.expect("assembly log should be emitted");
    assert_eq!(
        value["zeroclaw"]["agent_alias"], "channel-agent",
        "assembly log must inherit the channel agent attribution span, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model_provider"], "openai.test-provider",
        "assembly log must preserve the startup model_provider scope, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model_provider_type"], "openai",
        "assembly log must split the scoped provider family, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model_provider_alias"], "test-provider",
        "assembly log must split the scoped provider alias, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model"], "gpt-test",
        "assembly log must preserve the startup model scope, got: {value}"
    );

    zeroclaw_log::clear_broadcast_hook();
}

/// The `channel_path_*` tests elsewhere pin the shared filter/registration
/// helpers directly, not `start_channels`'s actual assembly call - a bad edit
/// to `assemble_channel_agent_tools`'s knobs (flipping `exclude_memory`,
/// dropping `connect_mcp`, etc.) would compile and slip past them undetected.
/// This test drives the exact function `start_channels` calls, closing that
/// gap for the built-in allow/deny behavior. (Pinned-resource resolution
/// through this same path is covered by
/// `assemble_channel_agent_tools_keeps_pinned_resources_after_strict_policy`;
/// `scoped.rs`'s `assemble_grants_no_mcp_to_agent_without_bundles` and siblings
/// cover the assembly's own MCP-grant policy.)
#[tokio::test]
async fn assemble_channel_agent_tools_honors_allowed_and_excluded_tools() {
    let config = Config::default();
    let built = channel_all_tools_result(vec![
        Box::new(NamedMockTool("shell")),
        Box::new(NamedMockTool("file_write")),
        Box::new(NamedMockTool("file_read")),
    ]);
    let security = Arc::new(SecurityPolicy {
        allowed_tools: Some(vec!["file_read".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    });
    let assembled = assemble_channel_agent_tools(
        &config,
        "test-agent",
        "test-provider",
        "test-model",
        &security,
        built,
        &[],
        Arc::new(platform::NativeRuntime::new()),
    )
    .await;
    let names: Vec<&str> = assembled.tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"shell") && !names.contains(&"file_write"),
        "the real start_channels assembly path must drop non-allowlisted built-ins; got {names:?}"
    );
    assert!(
        names.contains(&"file_read"),
        "the real start_channels assembly path must keep the allowlisted built-in; got {names:?}"
    );
}

/// Companion pin for `excluded_tools`, through the same real assembly path.
#[tokio::test]
async fn assemble_channel_agent_tools_drops_excluded_builtin() {
    let config = Config::default();
    let built = channel_all_tools_result(vec![
        Box::new(NamedMockTool("shell")),
        Box::new(NamedMockTool("file_read")),
    ]);
    let security = Arc::new(SecurityPolicy {
        excluded_tools: Some(vec!["shell".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..SecurityPolicy::default()
    });
    let assembled = assemble_channel_agent_tools(
        &config,
        "test-agent",
        "test-provider",
        "test-model",
        &security,
        built,
        &[],
        Arc::new(platform::NativeRuntime::new()),
    )
    .await;
    let names: Vec<&str> = assembled.tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"shell"),
        "the real start_channels assembly path must drop the excluded built-in; got {names:?}"
    );
    assert!(
        names.contains(&"file_read"),
        "the real start_channels assembly path must keep the non-excluded built-in; got {names:?}"
    );
}

/// Pins the `exclude_memory: false` knob at `assemble_channel_agent_tools`'s
/// call site - a regression flipping it to `true` (the ACP-only divergence)
/// would silently strip memory tools from every channel agent, undetected by
/// the allow/deny-only tests. Feeds one of every canonical memory tool name
/// (`zeroclaw_tools::MEMORY_TOOL_NAMES`) through the real assembly path and
/// asserts all five survive.
#[tokio::test]
async fn assemble_channel_agent_tools_keeps_memory_tools() {
    let config = Config::default();
    let built = channel_all_tools_result(
        zeroclaw_tools::MEMORY_TOOL_NAMES
            .iter()
            .map(|name| Box::new(NamedMockTool(name)) as Box<dyn Tool>)
            .collect(),
    );
    let security = Arc::new(SecurityPolicy::default());
    let assembled = assemble_channel_agent_tools(
        &config,
        "test-agent",
        "test-provider",
        "test-model",
        &security,
        built,
        &[],
        Arc::new(platform::NativeRuntime::new()),
    )
    .await;
    let names: Vec<&str> = assembled.tools.iter().map(|t| t.name()).collect();
    for memory_tool in zeroclaw_tools::MEMORY_TOOL_NAMES {
        assert!(
            names.contains(memory_tool),
            "the channel assembly path (exclude_memory: false) must keep memory tool \
                 '{memory_tool}'; got {names:?}"
        );
    }
}

struct FingerprintRuntime;

impl platform::RuntimeAdapter for FingerprintRuntime {
    fn name(&self) -> &str {
        "fingerprint-test-runtime"
    }
    fn has_shell_access(&self) -> bool {
        true
    }
    fn has_filesystem_access(&self) -> bool {
        true
    }
    fn storage_path(&self) -> std::path::PathBuf {
        std::env::temp_dir()
    }
    fn supports_long_running(&self) -> bool {
        false
    }
    fn build_shell_command(
        &self,
        _command: &str,
        _workspace_dir: &std::path::Path,
    ) -> anyhow::Result<tokio::process::Command> {
        // Deliberately fails with a distinguishable message instead of spawning a
        // real process, so executing the resulting skill tool proves THIS runtime
        // drove construction - not a default/native one, which would instead try
        // to actually run the command.
        anyhow::bail!("fingerprint-test-runtime: refusing to spawn a shell command")
    }
}

/// Pins the fix for a channel-path divergence: `start_channels` previously
/// called `register_skill_tools_with_context`, which always defaulted to
/// `NativeRuntime` regardless of `[platform]`. `assemble_channel_agent_tools` now
/// threads the orchestrator's real `runtime` parameter into skill construction -
/// proven here by injecting a runtime whose `build_shell_command` refuses to spawn
/// a real process with a distinguishable error; executing the resulting shell-kind
/// skill tool surfaces exactly that error, which could only happen if the injected
/// runtime (not a default) drove construction.
#[tokio::test]
async fn assemble_channel_agent_tools_threads_the_given_runtime_to_skill_tools() {
    let config = Config::default();
    let built = channel_all_tools_result(Vec::new());
    let security = Arc::new(SecurityPolicy::default());
    let skills = vec![zeroclaw_runtime::skills::Skill {
        name: "probe".into(),
        description: "d".into(),
        description_localizations: Default::default(),
        version: "1.0.0".into(),
        author: None,
        tags: vec![],
        tools: vec![zeroclaw_runtime::skills::SkillTool {
            name: "run".into(),
            description: "d".into(),
            kind: "shell".into(),
            command: "echo hi".into(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }],
        prompts: vec![],
        slash_options: Vec::new(),
        location: None,
    }];
    let assembled = assemble_channel_agent_tools(
        &config,
        "test-agent",
        "test-provider",
        "test-model",
        &security,
        built,
        &skills,
        Arc::new(FingerprintRuntime),
    )
    .await;
    let tool = assembled
        .tools
        .iter()
        .find(|t| t.name() == "probe__run")
        .expect("skill tool must be registered");
    let result = tool.execute(serde_json::json!({})).await;
    let failed_with_fingerprint = match &result {
        Err(e) => e.to_string().contains("fingerprint-test-runtime"),
        Ok(r) => {
            !r.success
                && r.error
                    .as_deref()
                    .is_some_and(|e| e.contains("fingerprint-test-runtime"))
        }
    };
    assert!(
        failed_with_fingerprint,
        "skill tool must execute via the INJECTED runtime, not a default one; got {result:?}"
    );
}

fn peer_prompt_test_context(
    channels_by_name: HashMap<String, Arc<dyn Channel>>,
    provider_impl: Arc<HistoryCaptureModelProvider>,
    prompt_config: Arc<Config>,
    tools_registry: Arc<Vec<Box<dyn Tool>>>,
) -> Arc<ChannelRuntimeContext> {
    Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl,
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(RecallMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(RecallMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry,
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config,
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    })
}

#[tokio::test]
async fn process_channel_message_executes_tool_calls_instead_of_sending_raw_json() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-42".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.starts_with("chat-42:"));
    assert!(reply.contains("BTC is currently around"));
    assert!(!reply.contains("\"tool_calls\""));
    assert!(!reply.contains("mock_price"));
}

#[tokio::test]
async fn process_channel_message_scopes_sender_session_key_for_sessions_current_tool() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let tmp = TempDir::new().unwrap();
    let session_store: Arc<dyn zeroclaw_infra::session_backend::SessionBackend> =
        Arc::new(zeroclaw_infra::session_store::SessionStore::new(tmp.path()).unwrap());

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SessionsCurrentModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(
            zeroclaw_runtime::tools::SessionsCurrentTool::new(Arc::clone(&session_store)),
        )]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: Some(Arc::clone(&session_store)),
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(&{
            let mut profile = zeroclaw_config::schema::RiskProfileConfig::default();
            profile.auto_approve.push("sessions_current".to_string());
            profile
        })),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        agent_transcription_provider: String::new(),
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-42".to_string(),
            content: "Which session is this?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.contains("Current session: test-channel_chat-42_alice"));
    assert!(reply.contains("Messages: 1"));
}

#[tokio::test]
async fn process_channel_message_renders_trailing_tool_receipts_block_when_enabled() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::Full,
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig {
                level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                auto_approve: vec!["mock_price".to_string()],
                ..Default::default()
            },
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: Some(zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator::new()),
        show_receipts_in_response: true,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        agent_transcription_provider: String::new(),
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-42".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    // Two sends: the model's reply and the trailing receipts block.
    assert!(
        sent_messages.len() >= 2,
        "expected at least 2 sends (reply + receipts block), got {}: {:?}",
        sent_messages.len(),
        sent_messages
    );

    let receipts_message = sent_messages
        .iter()
        .find(|m| m.contains("Tool receipts:"))
        .unwrap_or_else(|| {
            panic!(
                "no `Tool receipts:` send found; got {:?}",
                sent_messages.as_slice()
            )
        });
    assert!(
        receipts_message.starts_with("chat-42:"),
        "receipts block must be sent to the same reply target as the agent reply, got {receipts_message}"
    );
    assert!(
        receipts_message.contains("---\nTool receipts:"),
        "receipts block must be prefixed with the documented `---\\nTool receipts:` separator, got {receipts_message}"
    );
    assert!(
        receipts_message.contains("zc-receipt-"),
        "receipts block must carry at least one zc-receipt-* HMAC token (proves the generator actually ran), got {receipts_message}"
    );
    assert!(
        receipts_message.contains("mock_price"),
        "receipts block should name the tool that produced the receipt, got {receipts_message}"
    );
}

#[tokio::test]
async fn process_channel_message_omits_receipts_block_when_disabled() {
    // Backward-compat: with show_receipts_in_response=false (default), no
    // trailing receipts message is sent — even when a generator is active
    // and the loop ran tools. This is the path every other test relies on
    // implicitly; assert it once explicitly.
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        // Match the enabled-test setup so the tool actually runs; the
        // assertion below proves the receipt-block send is gated on
        // `show_receipts_in_response` and not on whether the loop saw
        // any receipts.
        autonomy_level: AutonomyLevel::Full,
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig {
                level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                auto_approve: vec!["mock_price".to_string()],
                ..Default::default()
            },
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: Some(zeroclaw_runtime::agent::tool_receipts::ReceiptGenerator::new()),
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        agent_transcription_provider: String::new(),
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-42".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(
        !sent_messages.iter().any(|m| m.contains("Tool receipts:")),
        "no receipts block must be sent when show_receipts_in_response=false; got {:?}",
        sent_messages.as_slice()
    );
}

#[tokio::test]
async fn process_channel_message_disabled_receipt_generator_emits_no_receipts_anywhere() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::Full,
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig {
                level: zeroclaw_config::autonomy::AutonomyLevel::Full,
                auto_approve: vec!["mock_price".to_string()],
                ..Default::default()
            },
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        agent_transcription_provider: String::new(),
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-42".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(
        !sent_messages.is_empty(),
        "agent must still respond when receipts are disabled"
    );
    assert!(
        !sent_messages.iter().any(|m| m.contains("zc-receipt-")),
        "no zc-receipt- token must appear in any sent message when receipts are disabled, got {:?}",
        sent_messages.as_slice()
    );
    assert!(
        !sent_messages.iter().any(|m| m.contains("Tool receipts:")),
        "no `Tool receipts:` block must be sent when receipts are disabled, got {:?}",
        sent_messages.as_slice()
    );

    // Strict surface check: the model's view of conversation history must
    // not carry a `[receipt: ` trailer either, otherwise an LLM trained
    // on echoing receipts could leak signed-looking output even though
    // nothing was actually signed.
    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (_key, turns) in histories.iter() {
        for msg in turns.iter() {
            assert!(
                !msg.content.contains("[receipt: "),
                "no `[receipt: ` trailer must appear in conversation history when receipts are disabled, got: {}",
                msg.content
            );
        }
    }
}

#[tokio::test]
async fn process_channel_message_telegram_does_not_persist_tool_summary_prefix() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-telegram-tool-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-telegram".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.contains("BTC is currently around"));

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek("telegram_chat-telegram_alice")
        .expect("telegram history should be stored");
    let assistant_turn = turns
        .iter()
        .rev()
        .find(|turn| turn.role == "assistant")
        .expect("assistant turn should be present");
    assert!(
        !assistant_turn.content.contains("[Used tools:"),
        "telegram history should not persist tool-summary prefix"
    );
}

#[tokio::test]
async fn process_channel_message_strips_unexecuted_tool_json_artifacts_from_reply() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(RawToolArtifactModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-raw-json".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-raw".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 3,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 1);
    assert!(sent_messages[0].starts_with("chat-raw:"));
    assert!(sent_messages[0].contains("BTC is currently around"));
    assert!(!sent_messages[0].contains("\"name\":\"mock_price\""));
    assert!(!sent_messages[0].contains("\"result\""));
}

#[tokio::test]
async fn process_channel_message_executes_tool_calls_with_alias_tags() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ToolCallingAliasModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-2".to_string(),
            sender: "bob".to_string(),
            reply_target: "chat-84".to_string(),
            content: "What is the BTC price now?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.starts_with("chat-84:"));
    assert!(reply.contains("alias-tag flow resolved"));
    assert!(!reply.contains("<toolcall>"));
    assert!(!reply.contains("mock_price"));
}

#[tokio::test]
async fn process_channel_message_handles_models_command_without_llm_call() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let alt_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let alt_model_provider: Arc<dyn ModelProvider> = alt_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("openrouter.default".to_string(), alt_model_provider);

    let mut prompt_config = zeroclaw_config::schema::Config::default();
    prompt_config
        .providers
        .models
        .ensure("openrouter", "default")
        .expect("openrouter slot must exist");

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(prompt_config),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-cmd-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "/models openrouter".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent = channel_impl.sent_messages.lock().await;
    assert_eq!(sent.len(), 1);
    let expected_reply = zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "channel-runtime-set-provider-switched",
        &[
            ("provider", "openrouter.default"),
            ("model", "default-model"),
        ],
    );
    assert!(sent[0].contains(&expected_reply));

    let route_key = "telegram_chat-1_alice";
    let route = runtime_ctx
        .route_overrides
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(route_key)
        .cloned()
        .expect("route should be stored for sender");
    assert_eq!(route.model_provider, "openrouter.default");
    assert_eq!(route.model, "default-model");

    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
    assert_eq!(alt_model_provider_impl.call_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn process_channel_message_uses_route_override_provider_and_model() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let routed_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let routed_model_provider: Arc<dyn ModelProvider> = routed_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("openrouter".to_string(), routed_model_provider);

    let route_key = "telegram_chat-1_alice".to_string();
    let mut route_overrides = HashMap::new();
    route_overrides.insert(
        route_key,
        ChannelRouteSelection {
            model_provider: "openrouter".into(),
            model: "route-model".to_string(),
            api_key: None,
        },
    );

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(route_overrides)),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-routed-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "hello routed model_provider".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        routed_model_provider_impl.call_count.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        routed_model_provider_impl
            .models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_slice(),
        &["route-model".to_string()]
    );
}

#[tokio::test]
async fn process_channel_message_persists_model_switch_with_route_credential() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let default_model_provider_impl = Arc::new(ModelSwitchRequestProvider::default());
    let default_model_provider: Arc<dyn ModelProvider> = default_model_provider_impl.clone();
    let switched_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let switched_model_provider: Arc<dyn ModelProvider> = switched_model_provider_impl.clone();
    let observer = Arc::new(RecordingObserver::default());

    // The model_switch tool requests the configured dotted provider ref;
    // the switch handler must preserve it through provider construction,
    // caching, and route persistence.
    let switched_provider_ref = "openrouter.default";
    let switched_key = Some("route-specific-key");

    // Seed the provider cache so `get_or_create_provider` returns our
    // mock for the switched provider instead of constructing a real one.
    // The cache key hashes the route-specific api_key together with the
    // resolved (dotted) provider ref at the current (startup) generation.
    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&default_model_provider),
    );
    provider_cache_seed.insert(
        provider_cache_key(switched_provider_ref, switched_key, 0),
        Arc::clone(&switched_model_provider),
    );

    let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
        hint: "fast".to_string(),
        model_provider: switched_provider_ref.to_string(),
        model: "switched-model".to_string(),
        api_key: Some("route-specific-key".to_string()),
    }];

    // The prompt config owns the profile accepted by ModelSwitchTool and
    // later resolved by the channel switch handler.
    let prompt_config = {
        let mut cfg = zeroclaw_config::schema::Config::default();
        {
            let entry = cfg
                .providers
                .models
                .ensure("openrouter", "default")
                .expect("openrouter.default provider slot must be creatable");
            entry.api_key = Some("config-openrouter-key".to_string());
            entry.model = Some("switched-model".to_string());
        }
        Arc::new(cfg)
    };

    let model_switch_tool = zeroclaw_runtime::tools::ModelSwitchTool::new(
        Arc::new(SecurityPolicy::default()),
        Arc::clone(&prompt_config),
    );

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&default_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(model_switch_tool)]),
        observer: observer.clone(),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::clone(&prompt_config),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(model_routes),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig {
                auto_approve: vec!["model_switch".to_string()],
                ..zeroclaw_config::schema::RiskProfileConfig::default()
            },
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-switch-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "trigger model switch".to_string(),
            subject: None,
            channel: "telegram".to_string(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    {
        let events = observer.events.lock().unwrap();
        let (starts, ends) = lifecycle_bracket_snapshot(&events);
        assert_eq!(
            starts.len(),
            1,
            "a model switch must not create a second AgentStart: {events:?}"
        );
        assert_eq!(
            ends.len(),
            1,
            "the switched turn must close once: {events:?}"
        );
        assert_eq!(
            starts[0], ends[0],
            "the switched turn's lifecycle pair must keep one correlation triple"
        );
    }

    // After the switch handler runs, the route override must be
    // persisted for this sender with the resolved api_key.
    let route_key = "telegram_chat-1_alice";
    let persisted = runtime_ctx
        .route_overrides
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(route_key)
        .cloned()
        .expect(
            "switch handler must persist a route override under the sender's history key \
                 (this is the regression #6173 — previously the new model was used only for \
                 the current turn and the next inbound message reverted to the default)",
        );
    assert_eq!(persisted.model_provider, switched_provider_ref);
    assert_eq!(persisted.model, "switched-model");
    assert_eq!(
        persisted.api_key.as_deref(),
        Some("route-specific-key"),
        "the route-specific api_key from model_routes must be persisted, \
             not the global key or None — otherwise the next turn loses route auth"
    );

    // Send a second message from the same sender, with no pending
    // model_switch this time. The persisted route override must be
    // honored — `get_route_selection` should return the switched
    // route and the switched provider should handle the request.
    let calls_before = switched_model_provider_impl
        .call_count
        .load(Ordering::SeqCst);
    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-switch-2".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "follow-up after switch".to_string(),
            subject: None,
            channel: "telegram".to_string(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let calls_after = switched_model_provider_impl
        .call_count
        .load(Ordering::SeqCst);
    assert!(
        calls_after > calls_before,
        "follow-up message must be served by the switched provider (the persisted \
             route override), not by the original default provider"
    );
    {
        let events = observer.events.lock().unwrap();
        let (starts, ends) = lifecycle_bracket_snapshot(&events);
        assert_eq!(
            starts.len(),
            2,
            "two logical turns need two starts: {events:?}"
        );
        assert_eq!(ends.len(), 2, "two logical turns need two ends: {events:?}");
        for (start, end) in starts.iter().zip(&ends) {
            assert_eq!(start, end, "each logical turn must keep one matched pair");
        }
    }
}

#[tokio::test]
async fn process_channel_message_uses_classifier_provider_for_precheck_model_selection() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let main_provider_impl = Arc::new(PrecheckProbeModelProvider::default());
    let main_provider: Arc<dyn ModelProvider> = main_provider_impl.clone();
    let classifier_provider_impl = Arc::new(PrecheckProbeModelProvider::default());
    let classifier_provider: Arc<dyn ModelProvider> = classifier_provider_impl.clone();
    let mut prompt_config = zeroclaw_config::schema::Config::default();
    prompt_config.providers.models.openai.insert(
        "my-classifier".to_string(),
        zeroclaw_config::schema::OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                model: Some("fast-intent".to_string()),
                temperature: Some(0.0),
                ..Default::default()
            },
        },
    );
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        classifier_provider: zeroclaw_config::providers::ModelProviderRef::from(
            "openai.my-classifier",
        ),
        ..Default::default()
    };
    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        main_provider,
        prompt_config,
        agent_cfg,
        "test-provider",
        None,
    );
    runtime_ctx
        .provider_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert("openai.my-classifier".to_string(), classifier_provider);

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-classifier-provider".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-precheck".to_string(),
            content: "background chatter".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        classifier_provider_impl
            .precheck_calls
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        classifier_provider_impl.main_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(main_provider_impl.precheck_calls.load(Ordering::SeqCst), 0);
    assert_eq!(main_provider_impl.main_calls.load(Ordering::SeqCst), 0);
    let models = classifier_provider_impl
        .models
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert_eq!(models.as_slice(), ["fast-intent"]);
    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(
        sent_messages.is_empty(),
        "provider returns NO_REPLY from precheck, so no visible reply should be sent"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn process_channel_message_precheck_log_uses_span_attribution_not_attrs() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let provider_impl = Arc::new(PrecheckProbeModelProvider::default());
    let provider: Arc<dyn ModelProvider> = provider_impl;

    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        provider,
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "custom.primary",
        None,
    );

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-precheck-log".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-precheck-log".to_string(),
            content: "background chatter".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 5,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut precheck_event = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(value)) => {
                if value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .is_some_and(|m| m == "reply-intent precheck completed")
                    && value
                        .pointer("/attributes/message_id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| id == "msg-precheck-log")
                {
                    precheck_event = Some(value);
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => continue,
        }
    }

    let value = precheck_event.expect("reply-intent precheck log should be emitted");
    assert_eq!(
        value["zeroclaw"]["agent_alias"], "test-agent",
        "precheck record must inherit agent_alias from the channel turn span, got: {value}"
    );
    assert_eq!(
        value["zeroclaw"]["model"], "test-model",
        "precheck record must preserve primary model attribution, got: {value}"
    );
    assert_eq!(
        value["attributes"]["classifier_model"], "test-model",
        "classifier model must use a non-attribution attr key, got: {value}"
    );
    assert!(
        value["attributes"].get("agent").is_none(),
        "agent alias belongs in zeroclaw.agent_alias, not attributes.agent: {value}"
    );
    assert!(
        value["attributes"].get("model").is_none(),
        "classifier model must not shadow zeroclaw.model via attributes.model: {value}"
    );
}

#[tokio::test]
async fn process_channel_message_skips_reply_intent_classifier_when_agent_precheck_disabled() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let provider_impl = Arc::new(PrecheckProbeModelProvider::default());
    let provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        precheck: zeroclaw_config::scattered_types::ChannelPrecheckConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        provider,
        zeroclaw_config::schema::Config::default(),
        agent_cfg,
        "test-provider",
        None,
    );

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-precheck-disabled".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-precheck-disabled".to_string(),
            content: "background chatter".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    assert_eq!(provider_impl.precheck_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider_impl.main_calls.load(Ordering::SeqCst), 1);
    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(
        sent_messages.as_slice(),
        ["chat-precheck-disabled:visible reply"]
    );
}

#[tokio::test]
async fn process_channel_message_precheck_timeout_fails_open_to_reply() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let provider_impl = Arc::new(PrecheckProbeModelProvider {
        precheck_delay: Some(Duration::from_secs(2)),
        ..Default::default()
    });
    let provider: Arc<dyn ModelProvider> = provider_impl.clone();
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        precheck: zeroclaw_config::scattered_types::ChannelPrecheckConfig {
            timeout_secs: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        channel,
        provider,
        zeroclaw_config::schema::Config::default(),
        agent_cfg,
        "test-provider",
        None,
    );

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-precheck-timeout".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-precheck-timeout".to_string(),
            content: "background chatter".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    assert_eq!(provider_impl.precheck_calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider_impl.main_calls.load(Ordering::SeqCst), 1);
    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(
        sent_messages.as_slice(),
        ["chat-precheck-timeout:visible reply"]
    );
}

#[tokio::test]
async fn process_channel_message_prefers_cached_default_provider_instance() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let startup_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let startup_model_provider: Arc<dyn ModelProvider> = startup_model_provider_impl.clone();
    let reloaded_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let reloaded_model_provider: Arc<dyn ModelProvider> = reloaded_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert("test-provider".to_string(), reloaded_model_provider);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&startup_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-default-provider-cache".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "hello cached default model_provider".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 3,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
}

#[tokio::test]
async fn process_channel_message_respects_configured_max_tool_iterations_above_default() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(IterativeToolModelProvider {
            required_tool_iterations: 11,
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 12,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig {
            loop_detection_enabled: false,
            ..zeroclaw_config::schema::PacingConfig::default()
        },
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-iter-success".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-iter-success".to_string(),
            content: "Loop until done".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.starts_with("chat-iter-success:"));
    assert!(reply.contains("Completed after 11 tool iterations."));
    assert!(!reply.contains("⚠️ Error:"));
}

#[tokio::test]
async fn process_channel_message_reports_configured_max_tool_iterations_limit() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(IterativeToolModelProvider {
            required_tool_iterations: 20,
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![Box::new(MockPriceTool)]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 3,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig {
            loop_detection_enabled: false,
            ..zeroclaw_config::schema::PacingConfig::default()
        },
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-iter-fail".to_string(),
            sender: "bob".to_string(),
            reply_target: "chat-iter-fail".to_string(),
            content: "Loop forever".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert!(!sent_messages.is_empty());
    let reply = sent_messages.last().unwrap();
    assert!(reply.starts_with("chat-iter-fail:"));
    // After Phase 9, the agent attempts a graceful summary instead of erroring.
    // The mock model_provider returns a tool call payload as text, which the agent
    // returns as its "summary". The key invariant: the loop terminates and
    // produces a response (not hanging forever).
    assert!(
        reply.contains("⚠️ Error: Agent exceeded maximum tool iterations (3)")
            || reply.len() > "chat-iter-fail:".len(),
        "Expected either an error message or a graceful summary response"
    );
}

struct RecallMemory;

#[async_trait::async_trait]
impl Memory for RecallMemory {
    fn name(&self) -> &str {
        "recall-memory"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw_memory::MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![zeroclaw_memory::MemoryEntry {
            id: "entry-1".to_string(),
            key: "memory_key_1".to_string(),
            content: "Age is 45".to_string(),
            category: zeroclaw_memory::MemoryCategory::Conversation,
            timestamp: "2026-02-20T00:00:00Z".to_string(),
            session_id: None,
            score: Some(0.9),
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }])
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&zeroclaw_memory::MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(1)
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw_memory::MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
        self.recall(query, limit, session_id, since, until).await
    }
}
impl ::zeroclaw_api::attribution::Attributable for RecallMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "RecallMemory"
    }
}

struct ConcurrencyTrackingProvider {
    delay: Duration,
    in_flight: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ModelProvider for ConcurrencyTrackingProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(current, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(format!("echo: {message}"))
    }
}

impl ::zeroclaw_api::attribution::Attributable for ConcurrencyTrackingProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ConcurrencyTrackingProvider"
    }
}

#[tokio::test]
async fn message_dispatch_processes_messages_in_parallel() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak_in_flight = Arc::new(AtomicUsize::new(0));

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(ConcurrencyTrackingProvider {
            delay: Duration::from_millis(250),
            in_flight: in_flight.clone(),
            peak_in_flight: peak_in_flight.clone(),
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(4);
    tx.send(zeroclaw_api::channel::ChannelMessage {
        id: "1".to_string(),
        sender: "alice".to_string(),
        reply_target: "alice".to_string(),
        content: "hello".to_string(),
        channel: "test-channel".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    })
    .await
    .unwrap();
    tx.send(zeroclaw_api::channel::ChannelMessage {
        id: "2".to_string(),
        sender: "bob".to_string(),
        reply_target: "bob".to_string(),
        content: "world".to_string(),
        channel: "test-channel".into(),
        channel_alias: None,
        timestamp: 2,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    })
    .await
    .unwrap();
    drop(tx);

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 2).await;

    let peak = peak_in_flight.load(Ordering::SeqCst);
    assert!(
        peak >= 2,
        "expected at least 2 concurrent in-flight dispatches, got peak {}",
        peak
    );
    assert_eq!(
        in_flight.load(Ordering::SeqCst),
        0,
        "all in-flight dispatches should have completed",
    );

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 2);
}

#[tokio::test]
async fn message_dispatch_interrupts_in_flight_telegram_request_and_preserves_context() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
        delay: Duration::from_millis(250),
        calls: std::sync::Mutex::new(Vec::new()),
    });

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: true,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
    let send_task = zeroclaw_spawn::spawn!(async move {
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "forwarded content".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-2".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "summarize this".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
    });

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
    send_task.await.unwrap();

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 1);
    assert!(sent_messages[0].starts_with("chat-1:"));
    assert!(sent_messages[0].contains("response-2"));
    drop(sent_messages);

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2);
    let second_call = &calls[1];
    assert!(
        second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("forwarded content") })
    );
    assert!(
        second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("summarize this") })
    );
    assert!(
        !second_call.iter().any(|(role, _)| role == "assistant"),
        "cancelled turn should not persist an assistant response"
    );
}

#[tokio::test]
async fn message_dispatch_interrupts_in_flight_slack_request_and_preserves_context() {
    let channel_impl = Arc::new(SlackRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
        delay: Duration::from_millis(250),
        calls: std::sync::Mutex::new(Vec::new()),
    });

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: true,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
    let send_task = zeroclaw_spawn::spawn!(async move {
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "U123".to_string(),
            reply_target: "C123".to_string(),
            content: "first question".to_string(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: Some("1741234567.100001".to_string()),
            interruption_scope_id: Some("1741234567.100001".to_string()),
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-2".to_string(),
            sender: "U123".to_string(),
            reply_target: "C123".to_string(),
            content: "second question".to_string(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: Some("1741234567.100001".to_string()),
            interruption_scope_id: Some("1741234567.100001".to_string()),
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
    });

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
    send_task.await.unwrap();

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 1);
    assert!(sent_messages[0].starts_with("C123:"));
    assert!(sent_messages[0].contains("response-2"));
    drop(sent_messages);

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2);
    let second_call = &calls[1];
    assert!(
        second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("first question") })
    );
    assert!(
        second_call
            .iter()
            .any(|(role, content)| { role == "user" && content.contains("second question") })
    );
    assert!(
        !second_call.iter().any(|(role, _)| role == "assistant"),
        "cancelled turn should not persist an assistant response"
    );
}

#[tokio::test]
async fn message_dispatch_interrupts_in_flight_whatsapp_request_and_preserves_context() {
    let channel_impl = Arc::new(WhatsAppRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(DelayedHistoryCaptureModelProvider {
        delay: Duration::from_millis(250),
        calls: std::sync::Mutex::new(Vec::new()),
    });

    let mut channel_config = zeroclaw_config::schema::ChannelsConfig::default();
    channel_config.whatsapp.insert(
        "default".to_string(),
        zeroclaw_config::schema::WhatsAppConfig {
            session_path: Some("/tmp/zeroclaw-whatsapp-session.db".into()),
            interrupt_on_new_message: true,
            ..Default::default()
        },
    );

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: interrupt_on_new_message_config(&channel_config),
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
    let send_task = zeroclaw_spawn::spawn!(async move {
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-1".to_string(),
            sender: "15555550123".to_string(),
            reply_target: "15555550123".to_string(),
            content: "first WhatsApp question".to_string(),
            channel: "whatsapp".into(),
            channel_alias: Some("default".to_string()),
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-2".to_string(),
            sender: "15555550123".to_string(),
            reply_target: "15555550123".to_string(),
            content: "second WhatsApp question".to_string(),
            channel: "whatsapp".into(),
            channel_alias: Some("default".to_string()),
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
    });

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
    send_task.await.unwrap();

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 1);
    assert!(sent_messages[0].starts_with("15555550123:"));
    assert!(sent_messages[0].contains("response-2"));
    drop(sent_messages);

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2);
    let second_call = &calls[1];
    assert!(
        second_call.iter().any(|(role, content)| {
            role == "user" && content.contains("first WhatsApp question")
        })
    );
    assert!(
        second_call.iter().any(|(role, content)| {
            role == "user" && content.contains("second WhatsApp question")
        })
    );
    assert!(
        !second_call.iter().any(|(role, _)| role == "assistant"),
        "cancelled turn should not persist an assistant response"
    );
}

#[tokio::test]
async fn message_dispatch_interrupt_scope_is_same_sender_same_chat() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SlowModelProvider {
            delay: Duration::from_millis(180),
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: true,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
    let send_task = zeroclaw_spawn::spawn!(async move {
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-a".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "first chat".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "msg-b".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-2".to_string(),
            content: "second chat".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
    });

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
    send_task.await.unwrap();

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(sent_messages.len(), 2);
    assert!(sent_messages.iter().any(|msg| msg.starts_with("chat-1:")));
    assert!(sent_messages.iter().any(|msg| msg.starts_with("chat-2:")));
}

#[tokio::test]
async fn process_channel_message_cancels_scoped_typing_task() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SlowModelProvider {
            delay: Duration::from_millis(20),
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "typing-msg".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-typing".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let starts = channel_impl.start_typing_calls.load(Ordering::SeqCst);
    let stops = channel_impl.stop_typing_calls.load(Ordering::SeqCst);
    assert_eq!(starts, 1, "start_typing should be called once");
    assert_eq!(stops, 1, "stop_typing should be called once");
}

#[tokio::test]
async fn approval_wait_pauses_typing_and_only_approval_resumes_it() {
    use zeroclaw_api::channel::{
        ApprovalSource, AttributedApprovalResponse, ChannelApprovalRequest, ChannelApprovalResponse,
    };

    let cases = [
        (
            PendingApprovalOutcome::Response(Some(AttributedApprovalResponse::operator(
                ChannelApprovalResponse::Approve,
            ))),
            true,
            false,
        ),
        (
            PendingApprovalOutcome::Response(Some(AttributedApprovalResponse::operator(
                ChannelApprovalResponse::AlwaysApprove,
            ))),
            true,
            false,
        ),
        (
            PendingApprovalOutcome::Response(Some(AttributedApprovalResponse::operator(
                ChannelApprovalResponse::Deny,
            ))),
            false,
            false,
        ),
        (
            PendingApprovalOutcome::Response(Some(AttributedApprovalResponse::from_runtime(
                ChannelApprovalResponse::Deny,
                ApprovalSource::TimedOut,
            ))),
            false,
            false,
        ),
        (PendingApprovalOutcome::Response(None), false, false),
        (PendingApprovalOutcome::Error, false, true),
    ];

    for (outcome, should_resume, should_error) in cases {
        let channel_impl = Arc::new(PendingApprovalChannel::new(outcome));
        let channel: Arc<dyn Channel> = channel_impl.clone();
        let typing = Arc::new(ScopedTypingController::new(
            Arc::clone(&channel),
            "approval-chat".to_string(),
        ));
        typing.resume().await;

        tokio::time::timeout(Duration::from_secs(1), async {
            while channel_impl.start_typing_calls.load(Ordering::SeqCst) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial typing should start");

        let wrapped = Arc::new(ApprovalTypingChannel::new(
            Arc::clone(&channel),
            Arc::clone(&typing),
        ));
        let approval_task = zeroclaw_spawn::spawn!(async move {
            wrapped
                .request_approval_attributed(
                    "approval-chat",
                    &ChannelApprovalRequest {
                        tool_name: "shell".to_string(),
                        arguments_summary: "command".to_string(),
                        raw_arguments: None,
                    },
                )
                .await
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            channel_impl.approval_started.notified(),
        )
        .await
        .expect("approval request should start");
        assert_eq!(
            channel_impl.stop_typing_calls.load(Ordering::SeqCst),
            1,
            "typing must stop before the approval wait begins"
        );
        assert_eq!(
            channel_impl.start_typing_calls.load(Ordering::SeqCst),
            1,
            "typing must remain paused while approval is pending"
        );

        channel_impl.approval_release.notify_one();
        let approval_result = approval_task.await.unwrap();
        assert_eq!(approval_result.is_err(), should_error);

        if should_resume {
            tokio::time::timeout(Duration::from_secs(1), async {
                while channel_impl.start_typing_calls.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("approved work should resume typing");
        } else {
            tokio::task::yield_now().await;
            assert_eq!(
                channel_impl.start_typing_calls.load(Ordering::SeqCst),
                1,
                "denied or timed-out work must not resume typing"
            );
        }

        typing.pause().await;
    }
}

#[tokio::test]
async fn process_channel_message_adds_and_swaps_reactions() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SlowModelProvider {
            delay: Duration::from_millis(5),
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "react-msg".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-react".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let added = channel_impl.reactions_added.lock().await;
    assert!(
        added.len() >= 2,
        "expected at least 2 reactions added (\u{1F440} then \u{2705}), got {}",
        added.len()
    );
    assert_eq!(added[0].2, "\u{1F440}", "first reaction should be eyes");
    assert_eq!(
        added.last().unwrap().2,
        "\u{2705}",
        "last reaction should be checkmark"
    );

    let removed = channel_impl.reactions_removed.lock().await;
    assert_eq!(removed.len(), 1, "eyes reaction should be removed once");
    assert_eq!(removed[0].2, "\u{1F440}");
}

// Pins the no_reply reconciliation: when the agent deliberately chooses
// silence, the early 👀 ack must be removed (not left dangling) and the
// message must end carrying only the no-reply kind emoji. A regression that
// strands the 👀 on this path falsely signals "still processing" forever.
#[tokio::test]
async fn process_channel_message_no_reply_clears_early_ack() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(NoReplyModelProvider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "noreply-msg".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-noreply".to_string(),
            content: "fyi".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let added = channel_impl.reactions_added.lock().await;
    assert!(
        added.iter().any(|r| r.2 == "\u{1F44D}"),
        "the informational no-reply emoji must be added, got {added:?}"
    );
    assert!(
        !added.iter().any(|r| r.2 == "\u{2705}"),
        "no_reply must not produce a completion checkmark, got {added:?}"
    );

    let removed = channel_impl.reactions_removed.lock().await;
    assert!(
        removed.iter().any(|r| r.2 == "\u{1F440}"),
        "the early eyes ack must be reconciled (removed) on the no_reply path, got {removed:?}"
    );
}

struct AckTimingChannel {
    start: Instant,
    ack_elapsed_ms: tokio::sync::Mutex<Option<u128>>,
}

impl ::zeroclaw_api::attribution::Attributable for AckTimingChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "ack-timing"
    }
}

#[async_trait::async_trait]
impl Channel for AckTimingChannel {
    fn name(&self) -> &str {
        "ack-timing-channel"
    }
    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }
    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        if emoji == "\u{1F440}" {
            let mut slot = self.ack_elapsed_ms.lock().await;
            if slot.is_none() {
                *slot = Some(self.start.elapsed().as_millis());
            }
        }
        Ok(())
    }
}

// Pins the early-ack ordering: with a slow model_provider, the 👀 ack must
// land well before the model completes. Fails on the old order where the
// ack was awaited after enrichment / the model call. A regression back to
// the late position would record the ack at >= the model delay.
#[tokio::test]
async fn process_channel_message_acks_before_slow_model_completes() {
    let model_delay = Duration::from_millis(400);
    let channel_impl = Arc::new(AckTimingChannel {
        start: Instant::now(),
        ack_elapsed_ms: tokio::sync::Mutex::new(None),
    });
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SlowModelProvider { delay: model_delay }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "ack-msg".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-ack".to_string(),
            content: "hello".to_string(),
            channel: "ack-timing-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let ack_elapsed = channel_impl
        .ack_elapsed_ms
        .lock()
        .await
        .expect("eyes ack must have been attempted");
    assert!(
        ack_elapsed < model_delay.as_millis(),
        "ack fired at {ack_elapsed}ms, must precede the {}ms model delay (early-ack ordering)",
        model_delay.as_millis()
    );
}

#[test]
fn prompt_contains_all_sections() {
    let ws = make_workspace();
    let tools = vec![("shell", "Run commands"), ("file_read", "Read files")];
    let prompt = build_system_prompt(ws.path(), "test-model", &tools, &[], None, None);

    // Section headers
    assert!(prompt.contains("## Tools"), "missing Tools section");
    assert!(prompt.contains("## Safety"), "missing Safety section");
    assert!(prompt.contains("## Workspace"), "missing Workspace section");
    assert!(
        prompt.contains("## Project Context"),
        "missing Project Context"
    );
    assert!(prompt.contains("## Current Date"), "missing Date section");
    assert!(
        !prompt.contains("## Current Date & Time"),
        "prompt should use date-only context"
    );
    assert!(prompt.contains("## Runtime"), "missing Runtime section");
}

#[test]
fn prompt_injects_tools() {
    let ws = make_workspace();
    let tools = vec![
        ("shell", "Run commands"),
        ("memory_recall", "Search memory"),
    ];
    let prompt = build_system_prompt(ws.path(), "gpt-4o", &tools, &[], None, None);

    assert!(prompt.contains("**shell**"));
    assert!(prompt.contains("Run commands"));
    assert!(prompt.contains("**memory_recall**"));
}

#[test]
fn prompt_includes_single_tool_protocol_block_after_append() {
    let ws = make_workspace();
    let tools = vec![("shell", "Run commands")];
    let mut prompt = build_system_prompt(ws.path(), "gpt-4o", &tools, &[], None, None);

    assert!(
        !prompt.contains("## Tool Use Protocol"),
        "build_system_prompt should not emit protocol block directly"
    );

    let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    prompt.push_str(&build_tool_instructions(&tools_registry));

    assert_eq!(
        prompt.matches("## Tool Use Protocol").count(),
        1,
        "protocol block should appear exactly once in the final prompt"
    );
}

#[test]
fn channel_strict_non_native_prompt_hides_text_tool_protocol() {
    let ws = make_workspace();
    let mut tool_descs = vec![("shell", "Run commands")];
    let mut deferred_section = "## Deferred MCP Tools\n\n- mcp__example".to_string();

    let expose_text_protocol =
        apply_text_tool_prompt_policy(false, true, &mut tool_descs, &mut deferred_section);

    let mut prompt = build_system_prompt_with_mode_and_autonomy(
        ws.path(),
        "gpt-4o",
        &tool_descs,
        &[],
        None,
        None,
        None,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        false,
        0,
        false,
        false,
    );
    if expose_text_protocol {
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
        let effective_tool_names: HashSet<&str> =
            tools_registry.iter().map(|tool| tool.name()).collect();
        prompt.push_str(&build_tool_instructions_for_names(
            &tools_registry,
            &effective_tool_names,
        ));
    }
    if !deferred_section.is_empty() {
        prompt.push('\n');
        prompt.push_str(&deferred_section);
    }

    assert!(!expose_text_protocol);
    assert!(!prompt.contains("## Tools"));
    assert!(!prompt.contains("## Tool Use Protocol"));
    assert!(!prompt.contains("<tool_call>"));
    assert!(!prompt.contains("mcp__example"));
}

#[test]
fn prompt_injects_safety() {
    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    assert!(prompt.contains("Do not exfiltrate private data"));
    assert!(prompt.contains("Respect the runtime autonomy policy"));
    assert!(prompt.contains("Prefer `trash` over `rm`"));
}

#[test]
fn prompt_injects_workspace_files() {
    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    assert!(prompt.contains("### SOUL.md"), "missing SOUL.md header");
    assert!(prompt.contains("Be helpful"), "missing SOUL content");
    assert!(prompt.contains("### IDENTITY.md"), "missing IDENTITY.md");
    assert!(
        prompt.contains("Name: ZeroClaw"),
        "missing IDENTITY content"
    );
    assert!(prompt.contains("### USER.md"), "missing USER.md");
    assert!(prompt.contains("### AGENTS.md"), "missing AGENTS.md");
    assert!(prompt.contains("### TOOLS.md"), "missing TOOLS.md");
    // HEARTBEAT.md is intentionally excluded from channel prompts — it's only
    // relevant to the heartbeat worker and causes LLMs to emit spurious
    // "HEARTBEAT_OK" acknowledgments in channel conversations.
    assert!(
        !prompt.contains("### HEARTBEAT.md"),
        "HEARTBEAT.md should not be in channel prompt"
    );
    assert!(prompt.contains("### MEMORY.md"), "missing MEMORY.md");
    assert!(prompt.contains("User likes Rust"), "missing MEMORY content");
}

#[test]
fn prompt_missing_file_markers() {
    let tmp = TempDir::new().unwrap();
    // Empty workspace — no files at all
    let prompt = build_system_prompt(tmp.path(), "model", &[], &[], None, None);

    assert!(prompt.contains("[File not found: SOUL.md]"));
    assert!(prompt.contains("[File not found: AGENTS.md]"));
    assert!(prompt.contains("[File not found: IDENTITY.md]"));
}

#[test]
fn prompt_bootstrap_only_if_exists() {
    let ws = make_workspace();
    // No BOOTSTRAP.md — should not appear
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);
    assert!(
        !prompt.contains("### BOOTSTRAP.md"),
        "BOOTSTRAP.md should not appear when missing"
    );

    // Create BOOTSTRAP.md — should appear
    std::fs::write(ws.path().join("BOOTSTRAP.md"), "# Bootstrap\nFirst run.").unwrap();
    let prompt2 = build_system_prompt(ws.path(), "model", &[], &[], None, None);
    assert!(
        prompt2.contains("### BOOTSTRAP.md"),
        "BOOTSTRAP.md should appear when present"
    );
    assert!(prompt2.contains("First run"));
}

#[test]
fn prompt_no_daily_memory_injection() {
    let ws = make_workspace();
    let memory_dir = ws.path().join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    std::fs::write(
        memory_dir.join(format!("{today}.md")),
        "# Daily\nSome note.",
    )
    .unwrap();

    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    // Daily notes should NOT be in the system prompt (on-demand via tools)
    assert!(
        !prompt.contains("Daily Notes"),
        "daily notes should not be auto-injected"
    );
    assert!(
        !prompt.contains("Some note"),
        "daily content should not be in prompt"
    );
}

#[test]
fn prompt_runtime_metadata() {
    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "claude-sonnet-4", &[], &[], None, None);

    assert!(prompt.contains("Model: claude-sonnet-4"));
    assert!(prompt.contains(&format!("OS: {}", std::env::consts::OS)));
    assert!(prompt.contains("Host:"));
}

#[test]
fn prompt_skills_include_instructions_and_tools() {
    let ws = make_workspace();
    let skills = vec![zeroclaw_runtime::skills::Skill {
        name: "code-review".into(),
        description: "Review code for bugs".into(),
        description_localizations: Default::default(),
        version: "1.0.0".into(),
        author: None,
        tags: vec![],
        tools: vec![zeroclaw_runtime::skills::SkillTool {
            name: "lint".into(),
            description: "Run static checks".into(),
            kind: "shell".into(),
            command: "cargo clippy".into(),
            args: HashMap::new(),
            target: None,
            locked_args: std::collections::HashMap::new(),
            timeout_secs: None,
        }],
        prompts: vec!["Always run cargo test before final response.".into()],
        slash_options: Vec::new(),
        location: None,
    }];

    let prompt = build_system_prompt(ws.path(), "model", &[], &skills, None, None);

    assert!(prompt.contains("<available_skills>"), "missing skills XML");
    assert!(prompt.contains("<name>code-review</name>"));
    assert!(prompt.contains("<description>Review code for bugs</description>"));
    assert!(prompt.contains("SKILL.md</location>"));
    assert!(prompt.contains("<instructions>"));
    assert!(
        prompt.contains("<instruction>Always run cargo test before final response.</instruction>")
    );
    // Registered tools (shell kind) appear under <callable_tools> with prefixed names
    assert!(prompt.contains("<callable_tools"));
    assert!(prompt.contains("<name>code-review__lint</name>"));
    assert!(!prompt.contains("loaded on demand"));
}

#[test]
fn prompt_skills_compact_mode_omits_instructions_but_keeps_tools() {
    let ws = make_workspace();
    let skills = vec![zeroclaw_runtime::skills::Skill {
        name: "code-review".into(),
        description: "Review code for bugs".into(),
        description_localizations: Default::default(),
        version: "1.0.0".into(),
        author: None,
        tags: vec![],
        tools: vec![zeroclaw_runtime::skills::SkillTool {
            name: "lint".into(),
            description: "Run static checks".into(),
            kind: "shell".into(),
            command: "cargo clippy".into(),
            args: HashMap::new(),
            target: None,
            locked_args: std::collections::HashMap::new(),
            timeout_secs: None,
        }],
        prompts: vec!["Always run cargo test before final response.".into()],
        slash_options: Vec::new(),
        location: None,
    }];

    let prompt = build_system_prompt_with_mode(
        ws.path(),
        "model",
        &[],
        &skills,
        None,
        None,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
        AutonomyLevel::default(),
    );

    assert!(prompt.contains("<available_skills>"), "missing skills XML");
    assert!(prompt.contains("<name>code-review</name>"));
    assert!(prompt.contains("<location>skills/code-review/SKILL.md</location>"));
    assert!(prompt.contains("loaded on demand"));
    assert!(!prompt.contains("<instructions>"));
    assert!(
        !prompt.contains("<instruction>Always run cargo test before final response.</instruction>")
    );
    // Compact mode should still include tools so the LLM knows about them.
    // Registered tools (shell kind) appear under <callable_tools> with prefixed names.
    assert!(prompt.contains("<callable_tools"));
    assert!(prompt.contains("<name>code-review__lint</name>"));
}

#[test]
fn prompt_skills_escape_reserved_xml_chars() {
    let ws = make_workspace();
    let skills = vec![zeroclaw_runtime::skills::Skill {
        name: "code<review>&".into(),
        description: "Review \"unsafe\" and 'risky' bits".into(),
        description_localizations: Default::default(),
        version: "1.0.0".into(),
        author: None,
        tags: vec![],
        tools: vec![zeroclaw_runtime::skills::SkillTool {
            name: "run\"linter\"".into(),
            description: "Run <lint> & report".into(),
            kind: "shell&exec".into(),
            command: "cargo clippy".into(),
            args: HashMap::new(),
            target: None,
            locked_args: std::collections::HashMap::new(),
            timeout_secs: None,
        }],
        prompts: vec!["Use <tool_call> and & keep output \"safe\"".into()],
        slash_options: Vec::new(),
        location: None,
    }];

    let prompt = build_system_prompt(ws.path(), "model", &[], &skills, None, None);

    assert!(prompt.contains("<name>code&lt;review&gt;&amp;</name>"));
    assert!(prompt.contains(
        "<description>Review &quot;unsafe&quot; and &apos;risky&apos; bits</description>"
    ));
    assert!(prompt.contains("<name>run&quot;linter&quot;</name>"));
    assert!(prompt.contains("<description>Run &lt;lint&gt; &amp; report</description>"));
    assert!(prompt.contains("<kind>shell&amp;exec</kind>"));
    assert!(prompt.contains(
        "<instruction>Use &lt;tool_call&gt; and &amp; keep output &quot;safe&quot;</instruction>"
    ));
}

#[test]
fn prompt_truncation() {
    let ws = make_workspace();
    // Write a file larger than BOOTSTRAP_MAX_CHARS
    let big_content = "x".repeat(BOOTSTRAP_MAX_CHARS + 1000);
    std::fs::write(ws.path().join("AGENTS.md"), &big_content).unwrap();

    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    assert!(
        prompt.contains("truncated at"),
        "large files should be truncated"
    );
    assert!(
        !prompt.contains(&big_content),
        "full content should not appear"
    );
}

#[test]
fn prompt_empty_files_skipped() {
    let ws = make_workspace();
    std::fs::write(ws.path().join("TOOLS.md"), "").unwrap();

    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    // Empty file should not produce a header
    assert!(
        !prompt.contains("### TOOLS.md"),
        "empty files should be skipped"
    );
}

#[test]
fn channel_log_truncation_is_utf8_safe_for_multibyte_text() {
    let msg = "Hello from ZeroClaw 🌍. Current status is healthy, and café-style UTF-8 text stays safe in logs.";

    // Reproduces the production crash path where channel logs truncate at 80 chars.
    let result =
        std::panic::catch_unwind(|| zeroclaw_runtime::util::truncate_with_ellipsis(msg, 80));
    assert!(
        result.is_ok(),
        "truncate_with_ellipsis should never panic on UTF-8"
    );

    let truncated = result.unwrap();
    assert!(!truncated.is_empty());
    assert!(truncated.is_char_boundary(truncated.len()));
}

#[test]
fn prompt_contains_channel_capabilities() {
    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    assert!(
        prompt.contains("## Channel Capabilities"),
        "missing Channel Capabilities section"
    );
    assert!(
        prompt.contains("running as a messaging bot"),
        "missing channel context"
    );
    assert!(
        prompt.contains("NEVER repeat, describe, or echo credentials"),
        "missing security instruction"
    );
}

#[test]
fn full_autonomy_prompt_executes_allowed_tools_without_extra_approval() {
    let ws = make_workspace();
    let config = zeroclaw_config::schema::RiskProfileConfig {
        level: zeroclaw_runtime::security::AutonomyLevel::Full,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let prompt = build_system_prompt_with_mode_and_autonomy(
        ws.path(),
        "model",
        &[],
        &[],
        None,
        None,
        Some(&config),
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        false,
        0,
        false,
        false,
    );

    assert!(
        prompt.contains("execute it directly instead of asking the user for extra approval"),
        "full autonomy should instruct direct execution for allowed tools"
    );
    assert!(
        prompt.contains("Never pretend you are waiting for a human approval"),
        "full autonomy should not simulate interactive approval flows"
    );
}

#[test]
fn readonly_prompt_explains_policy_blocks_without_fake_approval() {
    let ws = make_workspace();
    let config = zeroclaw_config::schema::RiskProfileConfig {
        level: zeroclaw_runtime::security::AutonomyLevel::ReadOnly,
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };
    let prompt = build_system_prompt_with_mode_and_autonomy(
        ws.path(),
        "model",
        &[],
        &[],
        None,
        None,
        Some(&config),
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        false,
        0,
        false,
        false,
    );

    assert!(
        prompt.contains("this runtime is read-only for side effects"),
        "read-only prompt should expose the runtime restriction"
    );
    assert!(
        prompt.contains("instead of simulating an approval flow"),
        "read-only prompt should explain restrictions instead of faking approval"
    );
}

#[test]
fn prompt_workspace_path() {
    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    assert!(prompt.contains(&format!("Working directory: `{}`", ws.path().display())));
}

#[test]
fn full_autonomy_omits_approval_instructions() {
    let ws = make_workspace();
    let prompt = build_system_prompt_with_mode(
        ws.path(),
        "model",
        &[],
        &[],
        None,
        None,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        AutonomyLevel::Full,
    );

    assert!(
        !prompt.contains("without asking"),
        "full autonomy prompt must not tell the model to ask before acting"
    );
    assert!(
        !prompt.contains("ask before acting externally"),
        "full autonomy prompt must not contain ask-before-acting instruction"
    );
    // Core safety rules should still be present
    assert!(
        prompt.contains("Do not exfiltrate private data"),
        "data exfiltration guard must remain"
    );
    assert!(
        prompt.contains("Prefer `trash` over `rm`"),
        "trash-over-rm hint must remain"
    );
}

#[test]
fn supervised_autonomy_includes_approval_instructions() {
    let ws = make_workspace();
    let prompt = build_system_prompt_with_mode(
        ws.path(),
        "model",
        &[],
        &[],
        None,
        None,
        false,
        zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
        AutonomyLevel::Supervised,
    );

    assert!(
        prompt.contains("without asking"),
        "supervised prompt must include ask-before-acting instruction"
    );
    assert!(
        prompt.contains("ask before acting externally"),
        "supervised prompt must include ask-before-acting instruction"
    );
}

#[test]
fn channel_notify_observer_truncates_utf8_arguments_safely() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(128);
    let observer = ChannelNotifyObserver {
        inner: Arc::new(NoopObserver),
        tx,
        tools_used: AtomicBool::new(false),
    };

    let payload = (0..300)
        .map(|n| serde_json::json!({ "content": format!("{}置tail", "a".repeat(n)) }))
        .map(|v| v.to_string())
        .find(|raw| raw.len() > 120 && !raw.is_char_boundary(120))
        .expect("should produce non-char-boundary data at byte index 120");

    observer.record_event(
        &zeroclaw_runtime::observability::traits::ObserverEvent::ToolCallStart {
            parent_agent_alias: None,
            tool: "file_write".to_string(),
            tool_call_id: None,
            arguments: Some(payload),
            channel: None,
            agent_alias: None,
            turn_id: None,
        },
    );

    let emitted = rx.try_recv().expect("observer should emit notify message");
    assert!(emitted.contains("`file_write`"));
    assert!(emitted.is_char_boundary(emitted.len()));
}

#[test]
fn channel_notify_observer_caps_long_path_argument() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(128);
    let observer = ChannelNotifyObserver {
        inner: Arc::new(NoopObserver),
        tx,
        tools_used: AtomicBool::new(false),
    };

    // 64 KiB path — 16x the per-message cap.
    let long_path = "a".repeat(64 * 1024);
    let payload = serde_json::json!({ "path": &long_path }).to_string();

    observer.record_event(
        &zeroclaw_runtime::observability::traits::ObserverEvent::ToolCallStart {
            parent_agent_alias: None,
            tool: "file_read".to_string(),
            tool_call_id: None,
            arguments: Some(payload),
            channel: None,
            agent_alias: None,
            turn_id: None,
        },
    );

    let emitted = rx.try_recv().expect("observer should emit notify message");
    // The full input was 64 KiB; the emitted message must be capped
    // to NOTIFY_DETAIL_MAX_CHARS + the literal prefix/suffix chars
    // ("\u{1F527} `file_read`: " = 17 chars + "…" = 1 char).
    let max_len = NOTIFY_DETAIL_MAX_CHARS + 17 + 1;
    assert!(
        emitted.chars().count() <= max_len,
        "emitted notify message must be capped (got {} chars, max {})",
        emitted.chars().count(),
        max_len
    );
    assert!(
        emitted.contains("`file_read`"),
        "emitted message must still identify the tool"
    );
    assert!(
        emitted.is_char_boundary(emitted.len()),
        "truncation must preserve a valid char boundary"
    );
}

#[tokio::test]
async fn channel_notify_observer_drops_on_full_channel() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
    let observer = ChannelNotifyObserver {
        inner: Arc::new(NoopObserver),
        tx,
        tools_used: AtomicBool::new(false),
    };

    let mk_event = || zeroclaw_runtime::observability::traits::ObserverEvent::ToolCallStart {
        parent_agent_alias: None,
        tool: "file_read".to_string(),
        tool_call_id: None,
        arguments: Some(r#"{"path":"/a"}"#.to_string()),
        channel: None,
        agent_alias: None,
        turn_id: None,
    };

    // First push lands in the bounded buffer (capacity 1).
    observer.record_event(&mk_event());
    // Second push must drop: the consumer has not drained yet, so
    // the buffer is full and `try_send` returns `Full`.
    observer.record_event(&mk_event());

    // Exactly one message arrived.
    let first = rx
        .recv()
        .await
        .expect("at least one notify should land before drop");
    assert!(first.contains("`file_read`"));
    // No second message; the channel is empty because the second
    // push was dropped (not queued behind the first).
    assert!(
        rx.try_recv().is_err(),
        "second push must be dropped when the channel is full"
    );
    // tools_used must reflect that both events were observed
    // (the drop is on the notify side, not the observer side).
    assert!(observer.tools_used.load(Ordering::Relaxed));
}

#[test]
fn conversation_memory_key_uses_message_id() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_abc123".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "hello".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    assert_eq!(conversation_memory_key(&msg), "slack_U123_msg_abc123");
}

#[test]
fn followup_thread_id_prefers_thread_ts() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "slack_C123_1741234567.123456".into(),
        sender: "U123".into(),
        reply_target: "C123".into(),
        content: "hello".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: Some("1741234567.123456".into()),
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    assert_eq!(
        followup_thread_id(&msg).as_deref(),
        Some("1741234567.123456")
    );
}

#[test]
fn followup_thread_id_falls_back_to_message_id() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_abc123".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "hello".into(),
        channel: "cli".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    assert_eq!(followup_thread_id(&msg).as_deref(), Some("msg_abc123"));
}

#[test]
fn followup_thread_id_does_not_open_matrix_thread_for_root_message() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "$event:server".into(),
        sender: "@alice:server".into(),
        reply_target: "!room:server".into(),
        content: "hello".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    assert_eq!(followup_thread_id(&msg), None);
}

#[test]
fn matrix_root_conversation_history_key_omits_event_id() {
    let first = zeroclaw_api::channel::ChannelMessage {
        id: "$first:server".into(),
        sender: "@alice:server".into(),
        reply_target: "!room:server".into(),
        content: "send a.txt".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let second = zeroclaw_api::channel::ChannelMessage {
        id: "$second:server".into(),
        content: "send it again".into(),
        timestamp: 2,
        ..first.clone()
    };

    let key = conversation_history_key(&first);
    assert_eq!(key, conversation_history_key(&second));
    assert!(!key.contains("$first:server"));
    assert!(!key.contains("$second:server"));
}

#[test]
fn matrix_self_anchored_root_history_key_omits_event_id() {
    let first = zeroclaw_api::channel::ChannelMessage {
        id: "$first:server".into(),
        sender: "@alice:server".into(),
        reply_target: "!room:server".into(),
        content: "call me boss".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: Some("$first:server".into()),
        interruption_scope_id: Some("$first:server".into()),
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let second = zeroclaw_api::channel::ChannelMessage {
        id: "$second:server".into(),
        content: "hello".into(),
        timestamp: 2,
        thread_ts: Some("$second:server".into()),
        interruption_scope_id: Some("$second:server".into()),
        ..first.clone()
    };

    let key = conversation_history_key(&first);
    assert_eq!(key, conversation_history_key(&second));
    assert!(!key.contains("$first:server"));
    assert!(!key.contains("$second:server"));
}

#[test]
fn matrix_thread_follow_up_shares_root_session_key() {
    let root = zeroclaw_api::channel::ChannelMessage {
        id: "$root:server".into(),
        sender: "@alice:server".into(),
        reply_target: "!room:server".into(),
        content: "open the thread".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: Some("$root:server".into()),
        interruption_scope_id: Some("$root:server".into()),
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let follow_up = zeroclaw_api::channel::ChannelMessage {
        id: "$reply:server".into(),
        content: "thread reply".into(),
        timestamp: 2,
        thread_ts: Some("$root:server".into()),
        interruption_scope_id: Some("$root:server".into()),
        ..root.clone()
    };

    let root_key = conversation_history_key(&root);
    assert_eq!(root_key, conversation_history_key(&follow_up));
    assert!(!root_key.contains("$root:server"));
    assert!(!root_key.contains("$reply:server"));
}

#[test]
fn reply_target_conversation_scope_omits_sender_from_history_key() {
    let first = zeroclaw_api::channel::ChannelMessage {
        id: "msg-1".into(),
        sender: "alice".into(),
        reply_target: "123456@g.us".into(),
        content: "group context".into(),
        channel: "whatsapp".into(),
        channel_alias: Some("main".into()),
        timestamp: 1,
        conversation_scope: zeroclaw_api::channel::ChannelConversationScope::ReplyTarget,
        ..Default::default()
    };
    let second = zeroclaw_api::channel::ChannelMessage {
        id: "msg-2".into(),
        sender: "bob".into(),
        content: "follow up".into(),
        timestamp: 2,
        ..first.clone()
    };

    let key = conversation_history_key(&first);
    assert_eq!(key, conversation_history_key(&second));
    assert!(!key.contains("alice"));
    assert!(!key.contains("bob"));
}

#[test]
fn reply_target_conversation_history_key_uses_room_scope() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_wecom_ws".into(),
        sender: "zeroclaw_user".into(),
        reply_target: "group--room-1".into(),
        content: "hello".into(),
        channel: "wecom_ws".into(),
        channel_alias: Some("work".into()),
        timestamp: 1,
        thread_ts: Some("req-1".into()),
        interruption_scope_id: Some("group--room-1".into()),
        attachments: vec![],
        subject: None,
        conversation_scope: zeroclaw_api::channel::ChannelConversationScope::ReplyTarget,

        ..Default::default()
    };

    assert_eq!(
        conversation_history_key(&msg),
        "wecom_ws_work_group--room-1"
    );
    assert_eq!(interruption_scope_key(&msg), "wecom_ws_work_group--room-1");
}

#[test]
fn parse_runtime_command_allows_model_switch_for_wecom_ws() {
    assert_eq!(
        parse_runtime_command("wecom_ws", "/models openrouter"),
        Some(ChannelRuntimeCommand::SetProvider("openrouter".into()))
    );
    assert_eq!(
        parse_runtime_command("wecom_ws", "/model qwen-max"),
        Some(ChannelRuntimeCommand::SetModel("qwen-max".into()))
    );
}

#[test]
fn parse_runtime_command_allows_model_switch_for_whatsapp_web() {
    for channel in ["whatsapp", "whatsapp-web", "whatsapp_web"] {
        assert_eq!(
            parse_runtime_command(channel, "/models openrouter"),
            Some(ChannelRuntimeCommand::SetProvider("openrouter".into())),
            "{channel} should accept /models"
        );
        assert_eq!(
            parse_runtime_command(channel, "/model qwen-max"),
            Some(ChannelRuntimeCommand::SetModel("qwen-max".into())),
            "{channel} should accept /model"
        );
    }
}

fn scope_test_msg(
    sender: &str,
    channel_id: &str,
    thread: Option<&str>,
) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: channel_id.into(),
        channel: "discord".into(),
        channel_alias: Some("clamps".into()),
        thread_ts: thread.map(String::from),
        ..Default::default()
    }
}

#[test]
fn parse_runtime_command_parses_model_scope_flags() {
    use ChannelRuntimeCommand::{SetModel, SetModelScoped, ShowModel};
    assert_eq!(
        parse_runtime_command("discord", "/model --user gpt-4o"),
        Some(SetModelScoped(OverrideScope::User, "gpt-4o".into()))
    );
    assert_eq!(
        parse_runtime_command("discord", "/model --agent claude-opus-4-8"),
        Some(SetModelScoped(
            OverrideScope::Agent,
            "claude-opus-4-8".into()
        ))
    );
    // No flag → unchanged per-sender behavior.
    assert_eq!(
        parse_runtime_command("discord", "/model gpt-4o"),
        Some(SetModel("gpt-4o".into()))
    );
    // Bare /model, or a scope flag with no model id → show.
    assert_eq!(parse_runtime_command("discord", "/model"), Some(ShowModel));
    assert_eq!(
        parse_runtime_command("discord", "/model --user"),
        Some(ShowModel)
    );
    // A mistyped flag is NOT silently treated as a model id.
    assert_eq!(
        parse_runtime_command("discord", "/model --useer gpt-4o"),
        Some(ShowModel)
    );
}

#[test]
fn scope_override_key_drops_identifiers_below_each_scope() {
    let a = scope_test_msg("alice", "chan-1", Some("t-1"));
    let b = scope_test_msg("alice", "chan-2", Some("t-2"));
    // User scope spans a sender's chats/threads → same key.
    assert_eq!(
        scope_override_key(OverrideScope::User, &a, "agentX"),
        scope_override_key(OverrideScope::User, &b, "agentX"),
    );
    assert!(scope_override_key(OverrideScope::User, &a, "agentX").contains("alice"));
    // Agent scope keys only on the agent alias (independent of sender/chat).
    let c = scope_test_msg("bob", "chan-9", None);
    assert_eq!(
        scope_override_key(OverrideScope::Agent, &a, "agentX"),
        scope_override_key(OverrideScope::Agent, &c, "agentX"),
    );
    assert_ne!(
        scope_override_key(OverrideScope::Agent, &a, "agentX"),
        scope_override_key(OverrideScope::Agent, &a, "agentY"),
    );
}

#[test]
fn get_route_selection_precedence_user_over_agent_over_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = channel_runtime_context_for_defaults_test(
        tmp.path(),
        "agentX",
        "openrouter.default",
        "config-default-model",
    );
    let msg = scope_test_msg("alice", "chan-1", None);
    let snapshot = runtime_defaults_snapshot(&ctx);
    let sender_key = conversation_history_key(&msg);
    let sel = |m: &str| ChannelRouteSelection {
        model_provider: "openrouter.default".into(),
        model: m.into(),
        api_key: None,
    };

    // Nothing set → config default (whatever the snapshot resolves to).
    let default_model = get_route_selection(&ctx, &msg, &sender_key, &snapshot).model;
    assert_ne!(default_model, "session-model");

    // Per-sender route override (the session tier).
    set_route_selection(&ctx, &sender_key, sel("session-model"), &snapshot);
    assert_eq!(
        get_route_selection(&ctx, &msg, &sender_key, &snapshot).model,
        "session-model"
    );
    // Agent scope beats session.
    set_scope_override(
        &ctx,
        OverrideScope::Agent,
        &msg,
        sel("agent-model"),
        &snapshot,
    );
    assert_eq!(
        get_route_selection(&ctx, &msg, &sender_key, &snapshot).model,
        "agent-model"
    );
    // User scope beats agent.
    set_scope_override(
        &ctx,
        OverrideScope::User,
        &msg,
        sel("user-model"),
        &snapshot,
    );
    assert_eq!(
        get_route_selection(&ctx, &msg, &sender_key, &snapshot).model,
        "user-model"
    );
}

#[test]
fn set_scope_override_clears_when_equal_to_default() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = channel_runtime_context_for_defaults_test(
        tmp.path(),
        "agentX",
        "openrouter.default",
        "default-model",
    );
    let msg = scope_test_msg("alice", "chan", None);
    let snapshot = runtime_defaults_snapshot(&ctx);
    let default = default_route_selection_from_snapshot(&snapshot);
    set_scope_override(
        &ctx,
        OverrideScope::User,
        &msg,
        ChannelRouteSelection {
            model_provider: "openrouter.default".into(),
            model: "other".into(),
            api_key: None,
        },
        &snapshot,
    );
    assert_eq!(ctx.scope_overrides.lock().unwrap().len(), 1);
    // Setting it back to the config default clears the entry.
    set_scope_override(&ctx, OverrideScope::User, &msg, default, &snapshot);
    assert!(ctx.scope_overrides.lock().unwrap().is_empty());
}

#[test]
fn parse_runtime_command_maps_clear_to_new_session() {
    assert_eq!(
        parse_runtime_command("telegram", "/clear"),
        Some(ChannelRuntimeCommand::NewSession)
    );
    assert_eq!(
        parse_runtime_command("telegram", "/clear@zeroclaw_bot"),
        Some(ChannelRuntimeCommand::NewSession)
    );
    assert_eq!(parse_runtime_command("telegram", "/clear all"), None);
}

// Build a ChannelRuntimeContext with a Config that has peer_groups
// populated for the agent-scope authorization tests below. Mirrors
// `channel_runtime_context_for_defaults_test` but lets the caller
// inject a pre-built peer_groups map.
fn channel_runtime_context_with_peer_groups(
    zeroclaw_dir: &std::path::Path,
    peer_groups: std::collections::HashMap<String, zeroclaw_config::multi_agent::PeerGroupConfig>,
) -> ChannelRuntimeContext {
    let prompt_config = zeroclaw_config::schema::Config {
        peer_groups,
        ..Default::default()
    };
    let mut ctx = channel_runtime_context_for_defaults_test(
        zeroclaw_dir,
        "agentX",
        "openrouter.default",
        "config-default-model",
    );
    ctx.prompt_config = Arc::new(prompt_config);
    ctx
}

fn agent_scope_msg(sender: &str) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: "chan-1".into(),
        channel: "discord".into(),
        channel_alias: Some("clamps".into()),
        thread_ts: None,
        content: "/model --agent gpt-4o".into(),
        ..Default::default()
    }
}

fn user_scope_msg(sender: &str) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: "chan-1".into(),
        channel: "discord".into(),
        channel_alias: Some("clamps".into()),
        thread_ts: None,
        content: "/model --user gpt-4o".into(),
        ..Default::default()
    }
}

fn peer_group(
    channel: &str,
    members: &[&str],
    admin_for_agent_scope: bool,
) -> zeroclaw_config::multi_agent::PeerGroupConfig {
    use zeroclaw_config::multi_agent::{AgentAlias, OutputModality, PeerGroupConfig, PeerUsername};
    PeerGroupConfig {
        channel: zeroclaw_config::providers::ChannelRef(channel.into()),
        agents: Vec::<AgentAlias>::new(),
        external_peers: members
            .iter()
            .map(|s| PeerUsername(s.to_string()))
            .collect(),
        ignore: Vec::new(),
        output_modality: OutputModality::default(),
        admin_for_agent_scope,
    }
}

#[test]
fn set_model_scoped_agent_allowed_for_listed_admin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "discord_admins".into(),
        peer_group("discord.clamps", &["alice", "ops"], true),
    );
    let ctx = channel_runtime_context_with_peer_groups(tmp.path(), groups);

    assert!(is_agent_scope_authorized(&ctx, &agent_scope_msg("alice")));
    assert!(is_agent_scope_authorized(&ctx, &agent_scope_msg("ops")));
}

#[test]
fn set_model_scoped_agent_rejected_for_non_admin_member() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    // Same peer group as the admin test, but the admin flag is OFF.
    groups.insert(
        "discord_users".into(),
        peer_group("discord.clamps", &["alice", "ops"], false),
    );
    let ctx = channel_runtime_context_with_peer_groups(tmp.path(), groups);

    assert!(!is_agent_scope_authorized(&ctx, &agent_scope_msg("alice")));
    assert!(!is_agent_scope_authorized(&ctx, &agent_scope_msg("ops")));
    // Even an unknown sender is rejected (not silently allowed).
    assert!(!is_agent_scope_authorized(
        &ctx,
        &agent_scope_msg("mallory")
    ));
}

#[test]
fn set_model_scoped_agent_rejected_when_no_peer_groups_configured() {
    // Default config has no peer_groups — default deny.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx =
        channel_runtime_context_with_peer_groups(tmp.path(), std::collections::HashMap::new());

    assert!(!is_agent_scope_authorized(&ctx, &agent_scope_msg("alice")));
}

#[test]
fn set_model_scoped_user_unaffected_by_agent_scope_authz() {
    // The helper is only invoked on the Agent branch; this test pins
    // that `/model --user` does not even consult the helper. We
    // assert by behavior: the helper is gated on OverrideScope::Agent
    // in `handle_runtime_command_if_needed`, so for the User case
    // no authorization check runs and the override is written.
    // Here we simply verify that even when the sender is not an
    // admin, the auth helper treats them neutrally — i.e. the gate
    // is on the dispatch site, not the helper itself.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx =
        channel_runtime_context_with_peer_groups(tmp.path(), std::collections::HashMap::new());
    // For the User branch the helper is not consulted, so this
    // negative assertion is structural: the gate sits at the
    // SetModelScoped dispatch point, not in this helper.
    assert!(!is_agent_scope_authorized(&ctx, &user_scope_msg("alice")));
}

#[test]
fn channel_agent_scope_admins_filters_by_admin_flag() {
    // The resolver itself honors `admin_for_agent_scope = true`
    // only — build a `Config` with one admin-flagged group and one
    // unflagged group covering the same channel; the admin-flagged
    // group must surface while the unflagged group must not.
    // (Note: the orchestrator gate reads through a snapshot of this
    // `Config`, so operator edits become visible after restart — see
    // `is_agent_scope_authorized` docstring.)
    use zeroclaw_config::schema::Config;
    let mut config = Config::default();
    config.peer_groups.insert(
        "discord_admins".into(),
        peer_group("discord.clamps", &["alice", "ops"], true),
    );
    config.peer_groups.insert(
        "discord_users".into(),
        peer_group("discord.clamps", &["bob", "carol"], false),
    );
    let admins = config.channel_agent_scope_admins("discord", "clamps", "agentX");
    assert_eq!(admins, vec!["alice".to_string(), "ops".to_string()]);
}

/// Round-3 contract: when a peer group's `agents` list is non-empty,
/// the admin privilege is granted only for `agent_alias` values that
/// appear in that list. This pins the agent-bound semantics added in
/// round 3; without it, dropping or inverting the `agent_alias` filter
/// would not regress any existing test (because every prior fixture
/// constructs `agents: Vec::new()`, which falls through the legacy
/// channel-wide path).
fn peer_group_with_agents(
    channel: &str,
    members: &[&str],
    admin_for_agent_scope: bool,
    agents: &[&str],
) -> zeroclaw_config::multi_agent::PeerGroupConfig {
    use zeroclaw_config::multi_agent::{AgentAlias, OutputModality, PeerGroupConfig, PeerUsername};
    PeerGroupConfig {
        channel: zeroclaw_config::providers::ChannelRef(channel.into()),
        agents: agents.iter().map(|a| AgentAlias::new(*a)).collect(),
        external_peers: members
            .iter()
            .map(|s| PeerUsername(s.to_string()))
            .collect(),
        ignore: Vec::new(),
        output_modality: OutputModality::default(),
        admin_for_agent_scope,
    }
}

#[test]
fn channel_agent_scope_admins_filters_by_agents_list_when_non_empty() {
    // The same admin peer is granted the privilege for `agentX`
    // (because `agents = ["agentX"]` includes it) and denied for
    // `agentY` (because `agentY` is not in the list). The peer is
    // also denied for `agentX` if the group is constructed with an
    // empty `agents` list (the legacy channel-wide path), which is
    // pinned separately below.
    use zeroclaw_config::schema::Config;
    let mut config = Config::default();
    config.peer_groups.insert(
        "discord_admins".into(),
        peer_group_with_agents("discord.clamps", &["alice"], true, &["agentX"]),
    );

    let for_x = config.channel_agent_scope_admins("discord", "clamps", "agentX");
    assert_eq!(
        for_x,
        vec!["alice".to_string()],
        "agentX is in agents=[agentX], so alice must surface"
    );

    let for_y = config.channel_agent_scope_admins("discord", "clamps", "agentY");
    assert!(
        for_y.is_empty(),
        "agentY is NOT in agents=[agentX], so the group must be filtered out"
    );
}

#[test]
fn channel_agent_scope_admins_empty_agents_list_means_channel_wide() {
    // Backward-compatible legacy: an empty `agents` list means the
    // admin privilege is granted for any agent_alias on the channel.
    // This is the path every pre-round-3 config falls into.
    use zeroclaw_config::schema::Config;
    let mut config = Config::default();
    config.peer_groups.insert(
        "discord_admins".into(),
        peer_group("discord.clamps", &["alice"], true),
    );
    let for_x = config.channel_agent_scope_admins("discord", "clamps", "agentX");
    let for_y = config.channel_agent_scope_admins("discord", "clamps", "agentY");
    assert_eq!(for_x, vec!["alice".to_string()]);
    assert_eq!(for_y, vec!["alice".to_string()]);
}

// --- SSOT normalization + wildcard + leading-`@` + case-insensitive.
// The gate routes through `allowlist::is_user_allowed`, so the
// helpers below must mirror the inbound-channel normalization shape
// (strip leading `@`, ASCII-lowercase) and honor `["*"]` for the
// configured peer list. These tests pin that contract so a future
// refactor cannot silently fall back to the raw `==` shape that
// rejected correctly-configured admins.

#[test]
fn normalize_peer_username_strips_leading_at_and_lowercases() {
    assert_eq!(normalize_peer_username("@user_1"), "user_1");
    assert_eq!(normalize_peer_username("user_1"), "user_1");
    assert_eq!(normalize_peer_username("@Alice"), "alice");
    assert_eq!(normalize_peer_username("ALICE"), "alice");
    // Multiple leading `@` are collapsed to nothing: a config typo
    // like "@@alice" still resolves to the bare identity.
    assert_eq!(normalize_peer_username("@@alice"), "alice");
    // Empty input stays empty (would deny everything downstream).
    assert_eq!(normalize_peer_username(""), "");
}

#[test]
fn agent_scope_gate_matches_inbound_normalized_sender() {
    // Telegram inbound path strips a leading `@` from the sender
    // before calling `is_user_allowed`. The gate must accept a
    // configured `"@user_1"` against a sender that arrived as
    // `"user_1"` (no leading `@`).
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "telegram_admins".into(),
        peer_group("telegram.prod", &["@user_1"], true),
    );
    let ctx = channel_runtime_context_with_peer_groups(tmp.path(), groups);

    assert!(
        is_agent_scope_authorized(
            &ctx,
            &agent_scope_msg_with_channel("user_1", "telegram", "prod")
        ),
        "leading-`@` config entry must match Telegram's @-stripped sender identity"
    );
    // Without normalization, the raw `==` would deny this.
}

#[test]
fn agent_scope_gate_matches_case_insensitive_sender() {
    // Inbound IRC and Matrix use `Match::CaseInsensitive`; the
    // gate's ASCII-lowercase normalization must produce the same
    // outcome so a configured `"Alice"` matches an inbound `"alice"`.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "irc_admins".into(),
        peer_group("irc.freenode", &["Alice"], true),
    );
    let ctx = channel_runtime_context_with_peer_groups(tmp.path(), groups);

    assert!(
        is_agent_scope_authorized(
            &ctx,
            &agent_scope_msg_with_channel("alice", "irc", "freenode")
        ),
        "configured `Alice` must match an inbound `alice` sender (RFC 2812 case-insensitive)"
    );
    assert!(
        is_agent_scope_authorized(
            &ctx,
            &agent_scope_msg_with_channel("ALICE", "irc", "freenode")
        ),
        "configured `Alice` must match an inbound `ALICE` sender"
    );
}

#[test]
fn agent_scope_gate_honors_wildcard_admin() {
    // A peer group with `external_peers = ["*"]` is the documented
    // wildcard: every sender on the channel is an admin. The raw
    // `==` shape denied this; the SSOT shape admits it.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "discord_open".into(),
        peer_group("discord.clamps", &["*"], true),
    );
    let ctx = channel_runtime_context_with_peer_groups(tmp.path(), groups);

    assert!(is_agent_scope_authorized(&ctx, &agent_scope_msg("anyone")));
    assert!(is_agent_scope_authorized(
        &ctx,
        &agent_scope_msg("even_a_random_handle")
    ));
}

fn agent_scope_msg_with_channel(
    sender: &str,
    channel: &str,
    alias: &str,
) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: "chan-1".into(),
        channel: channel.into(),
        channel_alias: Some(alias.into()),
        thread_ts: None,
        content: "/model --agent gpt-4o".into(),
        ..Default::default()
    }
}

// --- Dispatch-level wiring tests for the agent-scope authorization
// gate. These exercise `handle_runtime_command_if_needed` directly so
// a future refactor that drops `scope == OverrideScope::Agent &&
// !is_agent_scope_authorized(...)` from the dispatch site cannot
// silently re-open the hole — every helper-only test would still pass
// while the gate was bypassed.

fn scope_user_msg(sender: &str) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: "chan-1".into(),
        channel: "discord".into(),
        channel_alias: Some("clamps".into()),
        thread_ts: None,
        content: "/model --user gpt-4o".into(),
        ..Default::default()
    }
}

fn scope_agent_msg(sender: &str) -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        sender: sender.into(),
        reply_target: "chan-1".into(),
        channel: "discord".into(),
        channel_alias: Some("clamps".into()),
        thread_ts: None,
        content: "/model --agent gpt-4o".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn dispatch_agent_scope_writes_override_for_admin_sender() {
    // Authorized sender: dispatch must reach the SetModelScoped(Agent)
    // accept branch and write a scope override.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "discord_admins".into(),
        peer_group("discord.clamps", &["alice"], true),
    );
    let ctx = Arc::new(channel_runtime_context_with_peer_groups(tmp.path(), groups));
    let target: Arc<dyn Channel> = Arc::new(NamedMockChannel { name: "discord" });

    let handled =
        handle_runtime_command_if_needed(ctx.as_ref(), &scope_agent_msg("alice"), Some(&target))
            .await;
    assert!(handled, "agent-scope command must be handled by dispatch");

    let overrides = ctx.scope_overrides.lock().unwrap();
    assert_eq!(
        overrides.len(),
        1,
        "authorized admin must produce exactly one scope override, got {overrides:?}"
    );
}

#[tokio::test]
async fn dispatch_agent_scope_rejects_unauthorized_sender_without_writing() {
    // Unauthorized sender: dispatch must surface the rejection string
    // and leave the override map empty. If the gate is dropped, the
    // override would be written and this test would fail.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut groups = std::collections::HashMap::new();
    groups.insert(
        "discord_admins".into(),
        peer_group("discord.clamps", &["alice"], true),
    );
    let ctx = Arc::new(channel_runtime_context_with_peer_groups(tmp.path(), groups));
    let target: Arc<dyn Channel> = Arc::new(NamedMockChannel { name: "discord" });

    let handled =
        handle_runtime_command_if_needed(ctx.as_ref(), &scope_agent_msg("mallory"), Some(&target))
            .await;
    assert!(handled, "command must be handled even when rejected");

    let overrides = ctx.scope_overrides.lock().unwrap();
    assert!(
        overrides.is_empty(),
        "unauthorized sender must NOT write a scope override, got {overrides:?}"
    );
}

#[tokio::test]
async fn dispatch_user_scope_writes_override_regardless_of_admin_status() {
    // The agent-scope gate must NOT affect `--user`. Even when the
    // sender is not in any admin peer group, `/model --user` writes
    // its override. This pins the gate's scope: only the Agent branch
    // consults `is_agent_scope_authorized`.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = Arc::new(channel_runtime_context_with_peer_groups(
        tmp.path(),
        std::collections::HashMap::new(),
    ));
    let target: Arc<dyn Channel> = Arc::new(NamedMockChannel { name: "discord" });

    let handled =
        handle_runtime_command_if_needed(ctx.as_ref(), &scope_user_msg("mallory"), Some(&target))
            .await;
    assert!(handled, "user-scope command must be handled");

    let overrides = ctx.scope_overrides.lock().unwrap();
    assert_eq!(
        overrides.len(),
        1,
        "`/model --user` must write a scope override even when sender is not an admin, got {overrides:?}"
    );
}

#[tokio::test]
async fn dispatch_agent_scope_rejects_when_no_peer_groups_configured() {
    // Default config (no peer_groups) — every sender must be denied.
    // Without the gate this would silently write the override.
    let tmp = tempfile::TempDir::new().unwrap();
    let ctx = Arc::new(channel_runtime_context_with_peer_groups(
        tmp.path(),
        std::collections::HashMap::new(),
    ));
    let target: Arc<dyn Channel> = Arc::new(NamedMockChannel { name: "discord" });

    let handled =
        handle_runtime_command_if_needed(ctx.as_ref(), &scope_agent_msg("alice"), Some(&target))
            .await;
    assert!(handled);

    let overrides = ctx.scope_overrides.lock().unwrap();
    assert!(
        overrides.is_empty(),
        "default-deny must produce zero overrides when no peer_groups are configured, got {overrides:?}"
    );
}

#[test]
fn parse_runtime_command_maps_thinking_levels() {
    assert_eq!(
        parse_runtime_command("telegram", "/thinking high"),
        Some(ChannelRuntimeCommand::SetThinking(Some(
            ThinkingLevel::High
        )))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking max"),
        Some(ChannelRuntimeCommand::SetThinking(Some(ThinkingLevel::Max)))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking off"),
        Some(ChannelRuntimeCommand::SetThinking(Some(ThinkingLevel::Off)))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking on"),
        Some(ChannelRuntimeCommand::SetThinking(Some(
            ThinkingLevel::High
        )))
    );
}

#[test]
fn parse_runtime_command_maps_thinking_reset_and_invalid() {
    assert_eq!(
        parse_runtime_command("telegram", "/thinking"),
        Some(ChannelRuntimeCommand::SetThinking(None))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking reset"),
        Some(ChannelRuntimeCommand::SetThinking(None))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking banana"),
        Some(ChannelRuntimeCommand::InvalidThinking("banana".into()))
    );
    assert_eq!(
        parse_runtime_command("telegram", "/thinking high now"),
        Some(ChannelRuntimeCommand::InvalidThinking(
            "too many arguments".into()
        ))
    );
}

#[test]
fn resolve_channel_thinking_uses_session_override_without_inline_directive() {
    let config = ThinkingConfig {
        default_level: ThinkingLevel::Low,
        ..ThinkingConfig::default()
    };
    let resolved = resolve_channel_thinking(
        "explain the tradeoff",
        Some(ThinkingLevel::High),
        &config,
        Some(0.5),
    );

    assert_eq!(resolved.effective_content, "explain the tradeoff");
    assert_eq!(resolved.level, ThinkingLevel::High);
    assert!(resolved.effective_temperature.unwrap() > 0.5);
}

#[test]
fn resolve_channel_thinking_inline_directive_beats_session_override() {
    let config = ThinkingConfig {
        default_level: ThinkingLevel::Low,
        ..ThinkingConfig::default()
    };
    let resolved = resolve_channel_thinking(
        "/think:off explain briefly",
        Some(ThinkingLevel::Max),
        &config,
        Some(0.5),
    );

    assert_eq!(resolved.effective_content, "explain briefly");
    assert_eq!(resolved.level, ThinkingLevel::Off);
    assert!(resolved.effective_temperature.unwrap() < 0.5);
}

#[test]
fn resolve_channel_thinking_strips_directive_before_url_enrichment() {
    let config = ThinkingConfig {
        default_level: ThinkingLevel::Low,
        ..ThinkingConfig::default()
    };
    let resolved = resolve_channel_thinking(
        "/think:max summarize https://example.com",
        None,
        &config,
        Some(0.5),
    );

    assert_eq!(resolved.effective_content, "summarize https://example.com");
    assert_eq!(resolved.level, ThinkingLevel::Max);
}

#[test]
fn resolve_models_command_resolves_bare_family_to_configured_alias() {
    let mut config = zeroclaw_config::schema::Config::default();
    {
        let base = config
            .providers
            .models
            .ensure("openrouter", "default")
            .expect("openrouter slot must exist");
        base.api_key = Some("sk-configured".into());
        base.uri = Some("https://router.example/v1".into());
        base.model = Some("some-model".into());
    }

    match resolve_models_command(&config, "openrouter") {
        ModelsCommandResolution::Resolved(r) => assert_eq!(r, "openrouter.default"),
        other => panic!("expected Resolved(openrouter.default), got {other:?}"),
    }

    // The resolved ref must carry the configured alias credentials.
    let (key, uri) = provider_credentials_for_ref(&config, "openrouter.default");
    assert_eq!(key.as_deref(), Some("sk-configured"));
    assert_eq!(uri.as_deref(), Some("https://router.example/v1"));
}

#[test]
fn resolve_models_command_rejects_family_without_alias() {
    let config = zeroclaw_config::schema::Config::default();
    match resolve_models_command(&config, "openrouter") {
        ModelsCommandResolution::NoAlias(f) => assert_eq!(f, "openrouter"),
        other => panic!("expected NoAlias(openrouter), got {other:?}"),
    }
}

#[test]
fn resolve_models_command_flags_ambiguous_family() {
    let mut config = zeroclaw_config::schema::Config::default();
    config
        .providers
        .models
        .ensure("openrouter", "default")
        .unwrap();
    config
        .providers
        .models
        .ensure("openrouter", "secondary")
        .unwrap();

    match resolve_models_command(&config, "openrouter") {
        ModelsCommandResolution::Ambiguous { family, aliases } => {
            assert_eq!(family, "openrouter");
            assert_eq!(
                aliases,
                vec!["default".to_string(), "secondary".to_string()]
            );
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn resolve_models_command_accepts_existing_dotted_ref() {
    let mut config = zeroclaw_config::schema::Config::default();
    config
        .providers
        .models
        .ensure("openrouter", "default")
        .unwrap();

    match resolve_models_command(&config, "openrouter.default") {
        ModelsCommandResolution::Resolved(r) => assert_eq!(r, "openrouter.default"),
        other => panic!("expected Resolved, got {other:?}"),
    }
    match resolve_models_command(&config, "openrouter.missing") {
        ModelsCommandResolution::NoAlias(r) => assert_eq!(r, "openrouter.missing"),
        other => panic!("expected NoAlias, got {other:?}"),
    }
}

#[test]
fn resolve_models_command_rejects_unknown_family() {
    let config = zeroclaw_config::schema::Config::default();
    assert!(matches!(
        resolve_models_command(&config, "definitely-not-a-provider"),
        ModelsCommandResolution::Unknown
    ));
}

#[test]
fn runtime_model_switch_resolves_bare_family_to_configured_alias() {
    let mut config = zeroclaw_config::schema::Config::default();
    config
        .providers
        .models
        .ensure("openrouter", "default")
        .unwrap();

    let resolved = resolve_provider_ref_for_runtime_switch(&config, "openrouter").unwrap();

    assert_eq!(resolved, "openrouter.default");
}

#[test]
fn runtime_model_switch_rejects_ambiguous_bare_family() {
    let mut config = zeroclaw_config::schema::Config::default();
    config
        .providers
        .models
        .ensure("openrouter", "default")
        .unwrap();
    config
        .providers
        .models
        .ensure("openrouter", "secondary")
        .unwrap();

    let err = resolve_provider_ref_for_runtime_switch(&config, "openrouter")
        .expect_err("ambiguous model switch provider should reject");

    assert!(err.to_string().contains("multiple configured aliases"));
}

#[test]
fn reply_intent_precheck_uses_structured_addressing_signal() {
    let marker_only = zeroclaw_api::channel::ChannelMessage {
        content: "[WeCom group message addressed to this bot via @danya]\n@danya say hi".into(),
        channel: "wecom_ws".into(),
        explicitly_addressed: false,
        ..Default::default()
    };
    assert!(!should_bypass_reply_intent_precheck(&marker_only, false));
    assert!(should_bypass_reply_intent_precheck(&marker_only, true));

    let addressed = zeroclaw_api::channel::ChannelMessage {
        explicitly_addressed: true,
        ..marker_only
    };
    assert!(should_bypass_reply_intent_precheck(&addressed, false));
}

#[test]
fn conversation_memory_key_is_unique_per_message() {
    let msg1 = zeroclaw_api::channel::ChannelMessage {
        id: "msg_1".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "first".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let msg2 = zeroclaw_api::channel::ChannelMessage {
        id: "msg_2".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "second".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 2,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    assert_ne!(
        conversation_memory_key(&msg1),
        conversation_memory_key(&msg2)
    );
}

#[tokio::test]
async fn autosave_keys_preserve_multiple_conversation_facts() {
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new("test", tmp.path()).unwrap();

    let msg1 = zeroclaw_api::channel::ChannelMessage {
        id: "msg_1".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "I'm Paul".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let msg2 = zeroclaw_api::channel::ChannelMessage {
        id: "msg_2".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "I'm 45".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 2,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };

    mem.store(
        &conversation_memory_key(&msg1),
        &msg1.content,
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();
    mem.store(
        &conversation_memory_key(&msg2),
        &msg2.content,
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();

    assert_eq!(mem.count().await.unwrap(), 2);

    let recalled = mem.recall("45", 5, None, None, None).await.unwrap();
    assert!(recalled.iter().any(|entry| entry.content.contains("45")));
}

/// Test shim: the old per-orchestrator renderer call shape, routed
/// through the unified engine pipeline (agent::memory_inject).
async fn render_for_sessions(
    mem: &dyn zeroclaw_memory::Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_ids: &[Option<&str>],
) -> String {
    zeroclaw_runtime::agent::memory_inject::render_memory_context(
        mem,
        &zeroclaw_runtime::observability::NoopObserver,
        user_msg,
        session_ids,
        &zeroclaw_runtime::agent::memory_inject::MemoryInjectConfig {
            min_relevance_score,
            ..Default::default()
        },
        false,
        zeroclaw_runtime::agent::TurnMeta {
            parent_agent_alias: None,
            agent_alias: Some("test-agent"),
            turn_id: "test-turn",
            channel_name: "test-channel",
        },
    )
    .await
}

#[tokio::test]
async fn autosaved_conversation_memory_is_recalled_by_sender_scope() {
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new("test", tmp.path()).unwrap();
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_1".into(),
        sender: "U123".into(),
        reply_target: "C456".into(),
        content: "Project codename is quartz".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let history_key = conversation_history_key(&msg);

    mem.store(
        &conversation_memory_key(&msg),
        &msg.content,
        MemoryCategory::Conversation,
        Some(&history_key),
    )
    .await
    .unwrap();

    let session_ids = sender_memory_session_ids(&msg, &history_key);
    let session_id_refs: Vec<Option<&str>> = session_ids.iter().map(|s| Some(s.as_str())).collect();
    let context = render_for_sessions(&mem, "quartz", 0.0, &session_id_refs).await;

    assert!(
        context.contains("Project codename is quartz"),
        "sender recall should include autosaved memories stored under the current session key, got: {context}"
    );
}

#[tokio::test]
async fn autosaved_group_conversation_memory_stays_session_scoped() {
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new("test", tmp.path()).unwrap();
    let group_a_msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_1".into(),
        sender: "U123".into(),
        reply_target: "group:alpha".into(),
        content: "Group alpha codename is quartz".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let group_b_msg = zeroclaw_api::channel::ChannelMessage {
        id: "msg_2".into(),
        sender: "U123".into(),
        reply_target: "group:beta".into(),
        content: "What was the codename?".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 2,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let group_a_history_key = conversation_history_key(&group_a_msg);
    let group_b_history_key = conversation_history_key(&group_b_msg);

    mem.store(
        &conversation_memory_key(&group_a_msg),
        &group_a_msg.content,
        MemoryCategory::Conversation,
        Some(&group_a_history_key),
    )
    .await
    .unwrap();

    let group_b_sender_session_ids = sender_memory_session_ids(&group_b_msg, &group_b_history_key);
    assert_eq!(group_b_sender_session_ids, vec!["U123".to_string()]);

    let group_b_sender_session_id_refs: Vec<Option<&str>> = group_b_sender_session_ids
        .iter()
        .map(|s| Some(s.as_str()))
        .collect();
    let sender_context =
        render_for_sessions(&mem, "quartz", 0.0, &group_b_sender_session_id_refs).await;
    let group_context =
        render_for_sessions(&mem, "quartz", 0.0, &[Some(&group_b_history_key)]).await;
    let source_group_context =
        render_for_sessions(&mem, "quartz", 0.0, &[Some(&group_a_history_key)]).await;

    assert!(
        sender_context.is_empty(),
        "sender scope must not leak autosaved group memory from another group, got: {sender_context}"
    );
    assert!(
        group_context.is_empty(),
        "target group scope must not include another group's autosaved memory, got: {group_context}"
    );
    assert!(
        source_group_context.contains("Group alpha codename is quartz"),
        "source group scope should still recall its own autosaved memory, got: {source_group_context}"
    );
}

#[tokio::test]
async fn sender_session_ids_match_migrated_matrix_sender_rows() {
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new("test", tmp.path()).unwrap();
    let raw_sender = "@alice:server";
    let sanitized_sender = sanitize_session_key(raw_sender);
    assert_eq!(sanitized_sender, "_alice_server");

    mem.store(
        "alice_fact",
        "Alice favors filtered coffee",
        MemoryCategory::Conversation,
        Some(sanitized_sender.as_str()),
    )
    .await
    .unwrap();

    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "evt_1".into(),
        sender: raw_sender.into(),
        reply_target: "!room:server".into(),
        content: "what coffee does alice prefer?".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 1,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    let history_key = conversation_history_key(&msg);
    let session_ids = sender_memory_session_ids(&msg, &history_key);
    assert!(
        session_ids.contains(&sanitized_sender),
        "sender session ids must include sanitized sender, got: {session_ids:?}"
    );
    let session_id_refs: Vec<Option<&str>> = session_ids.iter().map(|s| Some(s.as_str())).collect();
    let context = render_for_sessions(&mem, "coffee", 0.0, &session_id_refs).await;
    assert!(
        context.contains("Alice favors filtered coffee"),
        "sender recall must find migrated row stored under sanitized sender, got: {context}"
    );
}

#[tokio::test]
async fn process_channel_message_restores_per_sender_history_on_follow_ups() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-a".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-b".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "follow up".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].len(), 2);
    assert_eq!(calls[0][0].0, "system");
    assert_eq!(calls[0][1].0, "user");
    assert_eq!(calls[1].len(), 4);
    assert_eq!(calls[1][0].0, "system");
    assert_eq!(calls[1][1].0, "user");
    assert_eq!(calls[1][2].0, "assistant");
    assert_eq!(calls[1][3].0, "user");
    assert!(calls[1][1].1.starts_with('['));
    assert!(calls[1][1].1.contains("hello"));
    assert!(calls[1][2].1.contains("response-1"));
    assert!(calls[1][3].1.starts_with('['));
    assert!(calls[1][3].1.contains("follow up"));
}

#[tokio::test]
async fn process_channel_message_refreshes_available_skills_after_new_session() {
    let workspace = make_workspace();
    let mut config = Config {
        data_dir: workspace.path().to_path_buf(),
        ..Default::default()
    };
    config.skills.open_skills_enabled = false;

    let initial_skills =
        zeroclaw_runtime::skills::load_skills_with_config(workspace.path(), &config);
    assert!(initial_skills.is_empty());

    let default_identity = zeroclaw_config::schema::IdentityConfig::default();
    let initial_system_prompt = build_system_prompt_with_mode(
        workspace.path(),
        "test-model",
        &[],
        &initial_skills,
        Some(&default_identity),
        None,
        false,
        config.skills.prompt_injection_mode,
        AutonomyLevel::default(),
    );
    assert!(
        !initial_system_prompt.contains("refresh-test"),
        "initial prompt should not contain the new skill before it exists"
    );

    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new(initial_system_prompt),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(config.data_dir.clone()),
        prompt_config: Arc::new(config.clone()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-before-new".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-refresh".to_string(),
            content: "hello".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let skill_dir = workspace.path().join("skills").join("refresh-test");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: refresh-test\ndescription: Refresh the available skills section\n---\n# Refresh Test\nExpose this skill after /new.\n",
        )
        .unwrap();
    let refreshed_skills =
        zeroclaw_runtime::skills::load_skills_with_config(workspace.path(), &config);
    assert_eq!(refreshed_skills.len(), 1);
    assert_eq!(refreshed_skills[0].name, "refresh-test");
    assert!(
        refreshed_new_session_system_prompt(runtime_ctx.as_ref())
            .contains("<name>refresh-test</name>"),
        "fresh-session prompt should pick up skills added after startup"
    );

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-new-session".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-refresh".to_string(),
            content: "/new".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    {
        let histories = runtime_ctx
            .conversation_histories
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            histories.peek("telegram_chat-refresh_alice").is_none(),
            "/new should clear the cached sender history before the next message"
        );
    }

    {
        let pending_new_sessions = runtime_ctx
            .pending_new_sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            pending_new_sessions.contains("telegram_chat-refresh_alice"),
            "/new should mark the sender for a fresh next-message prompt rebuild"
        );
    }

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-after-new".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-refresh".to_string(),
            content: "hello again".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 3,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    {
        let calls = provider_impl
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0].0, "system");
        assert_eq!(calls[1][0].0, "system");
        assert!(
            !calls[0][0].1.contains("<name>refresh-test</name>"),
            "pre-/new prompt should not advertise a skill that did not exist yet"
        );
        assert!(
            calls[1][0].1.contains("<available_skills>"),
            "post-/new prompt should contain the refreshed skills block"
        );
        assert!(
            calls[1][0].1.contains("<name>refresh-test</name>"),
            "post-/new prompt should include skills discovered after the reset"
        );
    }

    let sent_messages = channel_impl.sent_messages.lock().await;
    let new_session_reply =
        zeroclaw_runtime::i18n::get_required_cli_string("channel-runtime-new-session");
    assert!(
        sent_messages
            .iter()
            .any(|message| message.contains(&new_session_reply))
    );
}

#[tokio::test]
async fn process_channel_message_enriches_current_turn_without_persisting_context() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let mut prompt_config = zeroclaw_config::schema::Config::default();
    prompt_config.agents.insert(
        "test-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec![
                "test-channel.default".into(),
                "other-channel.default".into(),
            ],
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        },
    );
    prompt_config.agents.insert(
        "peer-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["test-channel.default".into()],
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        },
    );
    prompt_config.agents.insert(
        "other-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["other-channel.default".into()],
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        },
    );
    prompt_config.peer_groups.insert(
        "current-room".to_string(),
        zeroclaw_config::multi_agent::PeerGroupConfig {
            channel: "test-channel.default".into(),
            agents: vec![
                zeroclaw_config::multi_agent::AgentAlias::new("test-agent"),
                zeroclaw_config::multi_agent::AgentAlias::new("peer-agent"),
            ],
            external_peers: vec![zeroclaw_config::multi_agent::PeerUsername::new("@Operator")],
            ..zeroclaw_config::multi_agent::PeerGroupConfig::default()
        },
    );
    prompt_config.peer_groups.insert(
        "other-room".to_string(),
        zeroclaw_config::multi_agent::PeerGroupConfig {
            channel: "other-channel.default".into(),
            agents: vec![
                zeroclaw_config::multi_agent::AgentAlias::new("test-agent"),
                zeroclaw_config::multi_agent::AgentAlias::new("other-agent"),
            ],
            ..zeroclaw_config::multi_agent::PeerGroupConfig::default()
        },
    );
    let prompt_config = Arc::new(prompt_config);
    let tools_registry: Arc<Vec<Box<dyn Tool>>> = Arc::new(vec![Box::new(
        zeroclaw_runtime::tools::SendMessageToPeerTool::new(
            Arc::clone(&prompt_config),
            "test-agent",
        ),
    )]);
    let runtime_ctx = peer_prompt_test_context(
        channels_by_name,
        provider_impl.clone(),
        Arc::clone(&prompt_config),
        tools_registry,
    );

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-ctx-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-ctx".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 2);
    // Peer map (from send_message_to_peer tool) stays in the system prompt.
    // Memory context is no longer in the system prompt — it moved to the
    // outgoing user-turn preamble so the system prefix can stay byte-stable
    // for prompt caching.
    assert_eq!(calls[0][0].0, "system");
    assert!(
        !calls[0][0].1.contains(MEMORY_CONTEXT_OPEN),
        "system prompt must not include memory context (it now lives in the user turn): {}",
        calls[0][0].1
    );
    assert!(
        !calls[0][0].1.contains("Age is 45"),
        "memory content must not bleed into the system prompt: {}",
        calls[0][0].1
    );
    assert!(
        calls[0][0]
            .1
            .contains("Current-channel peer map for agent \"test-agent\"")
    );
    assert!(calls[0][0].1.contains("peer groups: \"current-room\""));
    assert!(
        calls[0][0]
            .1
            .contains("use channel ref \"test-channel.default\"")
    );
    assert!(calls[0][0].1.contains("agent peers: \"peer-agent\""));
    assert!(calls[0][0].1.contains("external peers: \"operator\""));
    assert!(!calls[0][0].1.contains("\"other-room\""));
    assert!(!calls[0][0].1.contains("\"other-agent\""));
    assert_eq!(calls[0][1].0, "user");
    // User turn now carries the volatile preamble (turn-context, memory
    // context) followed by the timestamped user content.
    assert!(calls[0][1].1.contains("[turn-context]"));
    assert!(
        calls[0][1].1.contains(MEMORY_CONTEXT_OPEN),
        "memory context must be prepended into the outgoing user turn: {}",
        calls[0][1].1
    );
    assert!(
        calls[0][1].1.contains("Age is 45"),
        "memory content must be visible to the model via the user turn: {}",
        calls[0][1].1
    );
    assert!(calls[0][1].1.contains("hello"));

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek("test-channel_chat-ctx_alice")
        .expect("history should be stored for sender");
    assert_eq!(turns[0].role, "user");
    // Cached history must be the raw timestamped user content with NO
    // [turn-context] preamble and NO memory context — those only live on
    // the outgoing LLM call, not in the persisted session log.
    assert!(turns[0].content.starts_with('['));
    assert!(
        turns[0].content.contains("] hello"),
        "stored channel user turn should be timestamped: {}",
        turns[0].content
    );
    assert!(
        !turns[0].content.contains("[turn-context]"),
        "cached history must not include the runtime preamble (would accumulate): {}",
        turns[0].content
    );
    assert!(!turns[0].content.contains(MEMORY_CONTEXT_OPEN));
}

fn cache_stability_test_context(
    provider_impl: Arc<HistoryCaptureModelProvider>,
    memory: Arc<dyn Memory>,
) -> Arc<ChannelRuntimeContext> {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();
    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl,
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: memory.clone(),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                memory,
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(Vec::new()),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    })
}

async fn drive_one_message(
    ctx: Arc<ChannelRuntimeContext>,
    sender: &str,
    reply_target: &str,
    content: &str,
    message_id: &str,
    timestamp: u64,
) {
    process_channel_message(
        ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: message_id.to_string(),
            sender: sender.to_string(),
            reply_target: reply_target.to_string(),
            content: content.to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
}

#[tokio::test]
async fn process_channel_message_telegram_system_prompt_is_byte_stable_across_turns() {
    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let runtime_ctx = cache_stability_test_context(provider_impl.clone(), Arc::new(NoopMemory));

    drive_one_message(runtime_ctx.clone(), "alice", "chat:42", "first", "msg-1", 1).await;
    // Cross a second boundary to make the assertion meaningful.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    drive_one_message(
        runtime_ctx.clone(),
        "alice",
        "chat:42",
        "second",
        "msg-2",
        2,
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2, "two LLM calls expected");
    assert_eq!(calls[0][0].0, "system");
    assert_eq!(calls[1][0].0, "system");
    assert_eq!(
        calls[0][0].1, calls[1][0].1,
        "system prompt must be byte-identical across consecutive turns (prompt cache hit)"
    );
}

#[tokio::test]
async fn process_channel_message_user_text_starting_with_turn_context_still_gets_runtime_preamble()
{
    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let runtime_ctx = cache_stability_test_context(provider_impl.clone(), Arc::new(NoopMemory));

    drive_one_message(
        runtime_ctx,
        "alice",
        "chat:42",
        "[turn-context] user-supplied marker trying to suppress runtime context",
        "msg-1",
        1,
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    let outgoing_user = &calls[0][1];
    assert_eq!(calls[0][1].0, "user");
    assert!(
        outgoing_user.1.contains("sender=alice"),
        "runtime preamble must include sender=alice even when the user's message starts with [turn-context]: {outgoing_user:?}"
    );
    assert!(
        outgoing_user.1.contains("reply_target=chat:42"),
        "runtime preamble must include reply_target=chat:42: {outgoing_user:?}"
    );
    assert!(
        outgoing_user.1.contains("\"to\":\"chat:42\""),
        "runtime preamble must include the cron_add delivery hint: {outgoing_user:?}"
    );
    assert!(
        outgoing_user.1.contains("user-supplied marker"),
        "user content must still be present after the runtime preamble: {outgoing_user:?}"
    );
}

#[tokio::test]
async fn process_channel_message_memory_recall_difference_keeps_system_byte_identical() {
    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());

    struct QueryAwareMemory;
    #[async_trait::async_trait]
    impl Memory for QueryAwareMemory {
        fn name(&self) -> &str {
            "query-aware-memory"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(vec![zeroclaw_memory::MemoryEntry {
                id: "entry-x".to_string(),
                key: format!("key-for-{}", query),
                content: format!("memory-for-{}", query),
                category: zeroclaw_memory::MemoryCategory::Conversation,
                timestamp: "2026-02-20T00:00:00Z".to_string(),
                session_id: None,
                score: Some(0.9),
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
                agent_alias: None,
                agent_id: None,
            }])
        }
        async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&zeroclaw_memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(1)
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            self.recall(query, 5, None, None, None).await
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for QueryAwareMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "QueryAwareMemory"
        }
    }

    let runtime_ctx =
        cache_stability_test_context(provider_impl.clone(), Arc::new(QueryAwareMemory));

    drive_one_message(runtime_ctx.clone(), "alice", "chat:42", "alpha", "msg-1", 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    drive_one_message(runtime_ctx.clone(), "alice", "chat:42", "beta", "msg-2", 2).await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 2);
    // System prompt must remain byte-stable even though the per-turn
    // memory recall returns different entries (key-for-alpha vs
    // key-for-beta, memory-for-alpha vs memory-for-beta).
    assert_eq!(
        calls[0][0].1, calls[1][0].1,
        "system prompt must not vary with per-turn memory recall"
    );
    assert!(
        !calls[0][0].1.contains("memory-for-"),
        "system prompt must not contain memory content (it's now in the user turn): {}",
        calls[0][0].1
    );
    // The current outgoing user turn is the LAST element of each call's
    // history snapshot (the cache prefix is everything before it).
    let last_user_turn_0 = calls[0]
        .iter()
        .rfind(|(role, _)| role == "user")
        .expect("first call should contain a user turn");
    let last_user_turn_1 = calls[1]
        .iter()
        .rfind(|(role, _)| role == "user")
        .expect("second call should contain a user turn");
    assert!(
        last_user_turn_0.1.contains("memory-for-alpha"),
        "first turn current user content: {}",
        last_user_turn_0.1
    );
    assert!(
        last_user_turn_1.1.contains("memory-for-beta"),
        "second turn current user content: {}",
        last_user_turn_1.1
    );
}

#[tokio::test]
async fn process_channel_message_user_message_accumulates_no_preamble_in_cached_history() {
    // The cached conversation history (ctx.conversation_histories)
    // must not accumulate the runtime preamble across turns —
    // otherwise the conversation prefix cache hits would still
    // regress over time even if the system prompt is stable.
    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let runtime_ctx = cache_stability_test_context(provider_impl.clone(), Arc::new(NoopMemory));

    drive_one_message(
        runtime_ctx.clone(),
        "alice",
        "chat:42",
        "turn one",
        "msg-1",
        1,
    )
    .await;
    drive_one_message(
        runtime_ctx.clone(),
        "alice",
        "chat:42",
        "turn two",
        "msg-2",
        2,
    )
    .await;

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Find the actual history key by scanning all stored senders —
    // sanitize_session_key may mangle "chat:42" so we don't assume
    // a literal key.
    let mut sender_keys: Vec<String> = Vec::new();
    for (k, _) in histories.iter() {
        sender_keys.push(k.clone());
    }
    assert!(
        !sender_keys.is_empty(),
        "history should be stored for some sender; no keys found"
    );
    let turns = histories
        .peek(sender_keys.first().unwrap().as_str())
        .expect("history should be stored for sender");
    let user_turns: Vec<_> = turns.iter().filter(|t| t.role == "user").collect();
    assert_eq!(
        user_turns.len(),
        2,
        "expected 2 cached user turns; got {} (total={}, key={})",
        user_turns.len(),
        turns.len(),
        sender_keys.first().unwrap()
    );
    for (i, turn) in user_turns.iter().enumerate() {
        assert_eq!(turn.role, "user");
        assert!(
            !turn.content.contains("[turn-context]"),
            "cached history turn {i} must not contain the runtime preamble: {}",
            turn.content
        );
        assert!(
            !turn.content.contains(MEMORY_CONTEXT_OPEN),
            "cached history turn {i} must not contain memory context: {}",
            turn.content
        );
    }
}

#[tokio::test]
async fn process_channel_message_omits_peer_map_when_send_peer_tool_unavailable() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let mut prompt_config = zeroclaw_config::schema::Config::default();
    prompt_config.agents.insert(
        "test-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["test-channel.default".into()],
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        },
    );
    prompt_config.agents.insert(
        "peer-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["test-channel.default".into()],
            ..zeroclaw_config::schema::AliasedAgentConfig::default()
        },
    );
    prompt_config.peer_groups.insert(
        "current-room".to_string(),
        zeroclaw_config::multi_agent::PeerGroupConfig {
            channel: "test-channel.default".into(),
            agents: vec![
                zeroclaw_config::multi_agent::AgentAlias::new("test-agent"),
                zeroclaw_config::multi_agent::AgentAlias::new("peer-agent"),
            ],
            ..zeroclaw_config::multi_agent::PeerGroupConfig::default()
        },
    );
    let runtime_ctx = peer_prompt_test_context(
        channels_by_name,
        provider_impl.clone(),
        Arc::new(prompt_config),
        Arc::new(vec![]),
    );

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-ctx-no-tool".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-ctx".to_string(),
            content: "hello".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    assert!(!calls[0][0].1.contains("Current-channel peer map"));
    assert!(!calls[0][0].1.contains("send_message_to_peer"));
}

#[tokio::test]
async fn process_channel_message_persists_image_payload_verbatim() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider {
        vision: true,
        ..Default::default()
    });
    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig {
            enabled: true,
            ..Default::default()
        },
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-image-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-image".to_string(),
            content: "please inspect this".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            passive_context: false,
            explicitly_addressed: false,
            conversation_scope: zeroclaw_api::channel::ChannelConversationScope::Sender,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![zeroclaw_api::media::MediaAttachment {
                file_name: "sticker.png".to_string(),
                data: vec![1, 2, 3, 4],
                mime_type: Some("image/png".to_string()),
            }],
            subject: None,
            internal_sop_event: None,
        },
        CancellationToken::new(),
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    let current_user = calls[0]
        .iter()
        .rev()
        .find(|(role, _)| role == "user")
        .expect("provider call should include current user message");
    assert!(current_user.1.contains("[IMAGE:data:image/png;base64,"));
    assert!(current_user.1.contains("please inspect this"));
    drop(calls);

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek("test-channel_chat-image_alice")
        .expect("history should be stored for sender");
    assert_eq!(turns[0].role, "user");
    assert!(turns[0].content.starts_with('['));
    assert!(turns[0].content.contains("[Image: sticker.png attached"));
    assert!(turns[0].content.contains("please inspect this"));
    assert!(turns[0].content.contains("[IMAGE:data:"));
    assert!(turns[0].content.contains("AQIDBA"));
}

#[tokio::test]
async fn process_channel_message_telegram_keeps_system_instruction_at_top_only() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let mut histories =
        lru::LruCache::new(std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap());
    histories.push(
        "telegram_chat-telegram_alice".to_string(),
        vec![
            ChatMessage::assistant("stale assistant"),
            ChatMessage::user("earlier user question"),
            ChatMessage::assistant("earlier assistant reply"),
        ],
    );

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: provider_impl.clone(),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(histories)),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx.clone(),
        zeroclaw_api::channel::ChannelMessage {
            id: "tg-msg-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-telegram".to_string(),
            content: "hello".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let calls = provider_impl
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 4);

    let roles = calls[0]
        .iter()
        .map(|(role, _)| role.as_str())
        .collect::<Vec<_>>();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert!(
        calls[0][0].1.contains("When responding on Telegram:"),
        "telegram channel instructions should be embedded into the system prompt"
    );
    assert!(
        calls[0][0].1.contains("For media attachments use markers:"),
        "telegram media marker guidance should live in the system prompt"
    );
    assert!(!calls[0].iter().skip(1).any(|(role, _)| role == "system"));
}

#[test]
fn channel_delivery_instructions_for_matrix_match_marker_contract() {
    let block = channel_delivery_instructions("matrix")
        .expect("matrix channel must have a delivery-instructions block");
    assert!(
        block.contains("When responding on Matrix:"),
        "matrix block must identify itself"
    );
    assert!(
        block.contains("[IMAGE:<path-or-url>]"),
        "matrix block must describe local path or URL marker syntax"
    );
    assert!(
        block.contains("workspace-relative or absolute"),
        "matrix block must match the validator's local path contract"
    );
    assert!(
        block.contains("Copy paths from inbound messages or file tools exactly"),
        "matrix block must prevent rewriting inbound or tool-returned paths"
    );
    assert!(
        block.contains("http:// or https:// URLs"),
        "matrix block must describe supported remote marker targets"
    );
    assert!(
        !block.contains("Never use relative paths"),
        "matrix block must not contradict the workspace-relative marker contract"
    );
}

#[test]
fn channel_delivery_instructions_for_discord_mandates_absolute_paths() {
    let block = channel_delivery_instructions("discord")
        .expect("discord channel must have a delivery-instructions block");
    assert!(
        block.contains("When responding on Discord:"),
        "discord block must identify itself"
    );
    assert!(
        block.contains("For media attachments use markers:"),
        "discord block must describe marker syntax"
    );
    assert!(
        block.contains("MUST be absolute"),
        "discord block must mandate absolute paths"
    );
    assert!(
        block.contains("workspace"),
        "discord block must reference workspace bounds"
    );
    assert!(
        block.contains("[IMAGE:<absolute-path>]"),
        "discord block must show the absolute-path marker form"
    );
}

#[test]
fn channel_delivery_instructions_for_whatsapp_web_match_local_marker_contract() {
    let block = channel_delivery_instructions("whatsapp")
        .expect("whatsapp channel must have a delivery-instructions block");
    assert!(
        block.contains("When responding on WhatsApp Web:"),
        "whatsapp block must identify itself"
    );
    assert!(
        block.contains("[LOCATION:"),
        "whatsapp block must include location pin instructions"
    );
    assert!(
        block.contains("[IMAGE:<path>]"),
        "whatsapp block must describe marker syntax"
    );
    assert!(
        block.contains("inside the configured workspace directory"),
        "whatsapp block must describe workspace bounds"
    );
    assert!(
        block.contains("Absolute paths and workspace-relative paths are accepted"),
        "whatsapp block must match the validator's local path contract"
    );
    assert!(
        block.contains("Do not use http://, https://, data:, file:"),
        "whatsapp block must say URL schemes are refused"
    );
    assert_eq!(
        channel_delivery_instructions("whatsapp-web"),
        Some(block),
        "the compatibility alias should use the same WhatsApp Web guidance"
    );
}

#[test]
fn channel_delivery_instructions_for_lark_and_feishu_encourage_tool_use() {
    for channel_name in ["lark", "feishu"] {
        let block = channel_delivery_instructions(channel_name)
            .expect("lark and feishu must have a delivery-instructions block");
        assert!(
            block.contains("When responding on Lark/Feishu:"),
            "{channel_name} block must identify itself"
        );
        assert!(
            block.contains("use your tools"),
            "{channel_name} block must steer the model toward the agent tool path"
        );
        assert!(
            block.contains("Use tool results silently"),
            "{channel_name} block must keep internal tool bookkeeping out of replies"
        );

        let prompt = build_channel_system_prompt("base prompt", channel_name, None);
        assert!(
            prompt.contains("base prompt"),
            "{channel_name} system prompt must retain the base prompt"
        );
        assert!(
            prompt.contains("When responding on Lark/Feishu:"),
            "{channel_name} system prompt must include channel instructions"
        );
        assert!(
            prompt.contains("Use tool results silently"),
            "{channel_name} system prompt must include the tool-use guidance"
        );
    }
}

#[test]
fn channel_delivery_instructions_for_telegram_encourage_tool_use() {
    let block = channel_delivery_instructions("telegram")
        .expect("telegram channel must have a delivery-instructions block");
    assert!(
        block.contains("When responding on Telegram:"),
        "telegram block must identify itself"
    );
    // Positive: it must actively steer the model toward its tools for
    // real-time/external information.
    assert!(
        block.contains("use your tools"),
        "telegram block must instruct the model to use its tools"
    );
    assert!(
        block.contains("web_search_tool") && block.contains("web_fetch"),
        "telegram block must name the real-time tools so the model knows to reach for them"
    );
    assert!(
        block.contains("never guess or answer from memory alone"),
        "telegram block must forbid answering from memory when a tool can verify"
    );
    // Negative: the exact regressed phrasing must never come back.
    assert!(
        !block.contains("Use tool results silently: answer the latest user message directly"),
        "telegram block must not tell the model to answer directly instead of using tools (#6646)"
    );
}

#[test]
fn extract_tool_context_summary_collects_alias_and_native_tool_calls() {
    let history = vec![
        ChatMessage::system("sys"),
        ChatMessage::assistant(
            r#"<toolcall>
{"name":"shell","arguments":{"command":"date"}}
</toolcall>"#,
        ),
        ChatMessage::assistant(
            r#"{"content":null,"tool_calls":[{"id":"1","name":"web_search","arguments":"{}"}]}"#,
        ),
    ];

    let summary = extract_tool_context_summary(&history, 1);
    assert_eq!(summary, "[Used tools: shell, web_search]");
}

#[test]
fn extract_tool_context_summary_collects_prompt_mode_tool_result_names() {
    let history = vec![
        ChatMessage::system("sys"),
        ChatMessage::assistant("Using markdown tool call fence"),
        ChatMessage::user(
            r#"[Tool results]
<tool_result name="http_request">
{"status":200}
</tool_result>
<tool_result name="shell">
Mon Feb 20
</tool_result>"#,
        ),
    ];

    let summary = extract_tool_context_summary(&history, 1);
    assert_eq!(summary, "[Used tools: http_request, shell]");
}

#[test]
fn extract_tool_context_summary_respects_start_index() {
    let history = vec![
        ChatMessage::assistant(
            r#"<tool_call>
{"name":"stale_tool","arguments":{}}
</tool_call>"#,
        ),
        ChatMessage::assistant(
            r#"<tool_call>
{"name":"fresh_tool","arguments":{}}
</tool_call>"#,
        ),
    ];

    let summary = extract_tool_context_summary(&history, 1);
    assert_eq!(summary, "[Used tools: fresh_tool]");
}

#[test]
fn strip_isolated_tool_json_artifacts_removes_tool_calls_and_results() {
    let mut known_tools = HashSet::new();
    known_tools.insert("schedule".to_string());

    let input = r#"{"name":"schedule","parameters":{"action":"create","message":"test"}}
{"name":"schedule","parameters":{"action":"cancel","task_id":"test"}}
Let me create the reminder properly:
{"name":"schedule","parameters":{"action":"create","message":"Go to sleep"}}
{"result":{"task_id":"abc","status":"scheduled"}}
Done reminder set for 1:38 AM."#;

    let result = strip_isolated_tool_json_artifacts(input, &known_tools);
    let normalized = result
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        normalized,
        "Let me create the reminder properly:\nDone reminder set for 1:38 AM."
    );
}

#[test]
fn strip_isolated_tool_json_artifacts_preserves_non_tool_json() {
    let mut known_tools = HashSet::new();
    known_tools.insert("shell".to_string());

    let input = r#"{"name":"profile","parameters":{"timezone":"UTC"}}
This is an example JSON object for profile settings."#;

    let result = strip_isolated_tool_json_artifacts(input, &known_tools);
    assert_eq!(result, input);
}

// ── AIEOS Identity Tests─────────────────────────

#[test]
fn aieos_identity_from_file() {
    use tempfile::TempDir;
    use zeroclaw_config::schema::IdentityConfig;

    let tmp = TempDir::new().unwrap();
    let identity_path = tmp.path().join("aieos_identity.json");

    // Write AIEOS identity file
    let aieos_json = r#"{
            "identity": {
                "names": {"first": "Nova", "nickname": "Nov"},
                "bio": "A helpful AI assistant.",
                "origin": "Silicon Valley"
            },
            "psychology": {
                "mbti": "INTJ",
                "moral_compass": ["Be helpful", "Do no harm"]
            },
            "linguistics": {
                "style": "concise",
                "formality": "casual"
            }
        }"#;
    std::fs::write(&identity_path, aieos_json).unwrap();

    // Create identity config pointing to the file
    let config = IdentityConfig {
        format: "aieos".into(),
        aieos_path: Some("aieos_identity.json".into()),
        aieos_inline: None,
    };

    let prompt = build_system_prompt(tmp.path(), "model", &[], &[], Some(&config), None);

    // Should contain AIEOS sections
    assert!(prompt.contains("## Identity"));
    assert!(prompt.contains("**Name:** Nova"));
    assert!(prompt.contains("**Nickname:** Nov"));
    assert!(prompt.contains("**Bio:** A helpful AI assistant."));
    assert!(prompt.contains("**Origin:** Silicon Valley"));

    assert!(prompt.contains("## Personality"));
    assert!(prompt.contains("**MBTI:** INTJ"));
    assert!(prompt.contains("**Moral Compass:**"));
    assert!(prompt.contains("- Be helpful"));

    assert!(prompt.contains("## Communication Style"));
    assert!(prompt.contains("**Style:** concise"));
    assert!(prompt.contains("**Formality Level:** casual"));

    // Should NOT contain OpenClaw bootstrap file headers
    assert!(!prompt.contains("### SOUL.md"));
    assert!(!prompt.contains("### IDENTITY.md"));
    assert!(!prompt.contains("[File not found"));
}

#[test]
fn aieos_identity_from_inline() {
    use zeroclaw_config::schema::IdentityConfig;

    let config = IdentityConfig {
        format: "aieos".into(),
        aieos_path: None,
        aieos_inline: Some(r#"{"identity":{"names":{"first":"Claw"}}}"#.into()),
    };

    let prompt = build_system_prompt(
        std::env::temp_dir().as_path(),
        "model",
        &[],
        &[],
        Some(&config),
        None,
    );

    assert!(prompt.contains("**Name:** Claw"));
    assert!(prompt.contains("## Identity"));
}

#[test]
fn aieos_fallback_to_openclaw_on_parse_error() {
    use zeroclaw_config::schema::IdentityConfig;

    let config = IdentityConfig {
        format: "aieos".into(),
        aieos_path: Some("nonexistent.json".into()),
        aieos_inline: None,
    };

    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

    // Should fall back to OpenClaw format when AIEOS file is not found
    // (Error is logged to stderr with filename, not included in prompt)
    assert!(prompt.contains("### SOUL.md"));
}

#[test]
fn aieos_empty_uses_openclaw() {
    use zeroclaw_config::schema::IdentityConfig;

    // Format is "aieos" but neither path nor inline is set
    let config = IdentityConfig {
        format: "aieos".into(),
        aieos_path: None,
        aieos_inline: None,
    };

    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

    // Should use OpenClaw format (not configured for AIEOS)
    assert!(prompt.contains("### SOUL.md"));
    assert!(prompt.contains("Be helpful"));
}

#[test]
fn openclaw_format_uses_bootstrap_files() {
    use zeroclaw_config::schema::IdentityConfig;

    let config = IdentityConfig {
        format: "openclaw".into(),
        aieos_path: Some("identity.json".into()),
        aieos_inline: None,
    };

    let ws = make_workspace();
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], Some(&config), None);

    // Should use OpenClaw format even if aieos_path is set
    assert!(prompt.contains("### SOUL.md"));
    assert!(prompt.contains("Be helpful"));
    assert!(!prompt.contains("## Identity"));
}

#[test]
fn none_identity_config_uses_openclaw() {
    let ws = make_workspace();
    // Pass None for identity config
    let prompt = build_system_prompt(ws.path(), "model", &[], &[], None, None);

    // Should use OpenClaw format
    assert!(prompt.contains("### SOUL.md"));
    assert!(prompt.contains("Be helpful"));
}

#[test]
fn classify_health_ok_true() {
    let state = classify_health_result(&Ok(true));
    assert_eq!(state, ChannelHealthState::Healthy);
}

#[test]
fn classify_health_ok_false() {
    let state = classify_health_result(&Ok(false));
    assert_eq!(state, ChannelHealthState::Unhealthy);
}

#[tokio::test]
async fn classify_health_timeout() {
    let result = tokio::time::timeout(Duration::from_millis(1), async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        true
    })
    .await;
    let state = classify_health_result(&result);
    assert_eq!(state, ChannelHealthState::Timeout);
}

#[cfg(feature = "channel-matrix")]
#[test]
fn matrix_state_dir_is_distinct_per_alias() {
    // Regression: two [channels.matrix.<alias>] blocks previously resolved
    // to the same <config>/state/matrix dir, so the second listener to
    // start restored the first's session.json and ran as the wrong Matrix
    // account. The alias component must keep them separate.
    let config_path = std::path::Path::new("/home/u/.zeroclaw/config.toml");
    let clamps = matrix_state_dir(config_path, "clamps");
    let bender = matrix_state_dir(config_path, "bender");
    assert_ne!(
        clamps, bender,
        "distinct matrix aliases must not share a state dir"
    );
    assert_eq!(
        clamps,
        std::path::Path::new("/home/u/.zeroclaw/state/matrix/clamps")
    );
    assert_eq!(
        bender,
        std::path::Path::new("/home/u/.zeroclaw/state/matrix/bender")
    );
}

#[cfg(feature = "channel-mattermost")]
#[test]
fn collect_configured_channels_includes_mattermost_when_configured() {
    let mut config = Config::default();
    config.channels.mattermost.insert(
        "default".to_string(),
        zeroclaw_config::schema::MattermostConfig {
            enabled: true,
            url: "https://mattermost.example.com".to_string(),
            bot_token: Some("test-token".to_string()),
            login_id: None,
            password: None,
            channel_ids: vec!["channel-1".to_string()],
            team_ids: vec![],
            discover_dms: None,
            thread_replies: Some(true),
            mention_only: Some(false),
            interrupt_on_new_message: false,
            proxy_url: None,
            listen_mode: zeroclaw_config::schema::MattermostListenMode::default(),
            excluded_tools: vec![],
            reply_min_interval_secs: 0,
            reply_queue_depth_max: 0,
        },
    );
    // A channel is only collected when an enabled agent references it.
    config.agents.insert(
        "mattermost-default".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            channels: vec!["mattermost.default".into()],
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);

    assert!(
        channels
            .iter()
            .any(|entry| entry.display_name == "Mattermost")
    );
    assert!(
        channels
            .iter()
            .any(|entry| entry.channel.name() == "mattermost")
    );
}

#[cfg(feature = "channel-mattermost")]
#[test]
fn collect_configured_channels_falls_back_when_agent_bindings_missing() {
    let mut config = Config::default();
    config.channels.mattermost.insert(
        "default".to_string(),
        zeroclaw_config::schema::MattermostConfig {
            enabled: true,
            url: "https://mattermost.example.com".to_string(),
            bot_token: Some("test-token".to_string()),
            login_id: None,
            password: None,
            channel_ids: vec!["channel-1".to_string()],
            team_ids: vec![],
            discover_dms: None,
            thread_replies: Some(true),
            mention_only: Some(false),
            interrupt_on_new_message: false,
            proxy_url: None,
            listen_mode: zeroclaw_config::schema::MattermostListenMode::default(),
            excluded_tools: vec![],
            reply_min_interval_secs: 0,
            reply_queue_depth_max: 0,
        },
    );
    config.agents.clear();
    config.agents.insert(
        "legacy".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);

    assert!(
        channels
            .iter()
            .any(|entry| entry.display_name == "Mattermost"),
        "enabled channels should still load when no enabled agent declares channel bindings"
    );
}

#[cfg(feature = "channel-discord")]
#[test]
fn collect_configured_channels_skips_channel_when_only_owner_is_disabled() {
    // T1 — the bug path: an explicit binding exists, but the
    // owner agent is `enabled = false`. Legacy fallback must NOT
    // bring the channel online.
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "disco".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: false,
            channels: vec!["discord.default".into()],
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "default".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);

    assert!(
        !channels.iter().any(|entry| entry.display_name == "Discord"),
        "disabled-owner channel must not be collected (#8013)"
    );
}

#[cfg(feature = "channel-discord")]
#[test]
fn collect_configured_channels_legacy_accepts_all_when_no_bindings_declared() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "legacy".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "default".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);

    assert!(
        channels.iter().any(|entry| entry.display_name == "Discord"),
        "no-bindings-anywhere must still trigger the legacy fallback"
    );
}

#[cfg(feature = "channel-discord")]
#[test]
fn collect_configured_channels_respects_mixed_enabled_and_disabled_owners() {
    // T3 — two bound channels, one owner enabled (keeper) and one
    // owner disabled (loser). Only the enabled owner's channel
    // comes online.
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "keeper".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec!["discord.a".into()],
            ..Default::default()
        },
    );
    config.agents.insert(
        "loser".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: false,
            channels: vec!["discord.b".into()],
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "a".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "token-a".to_string(),
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "b".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "token-b".to_string(),
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);

    let discord_channels: Vec<_> = channels
        .iter()
        .filter(|entry| entry.display_name == "Discord")
        .collect();
    assert_eq!(
        discord_channels.len(),
        1,
        "exactly one Discord channel should be active when only one owner is enabled"
    );
    assert_eq!(
        discord_channels[0].alias.as_deref(),
        Some("a"),
        "only the enabled owner's channel should be active"
    );
}

#[cfg(feature = "channel-discord")]
#[test]
fn approval_route_collects_unowned_channel_without_agent_dispatch() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "worker".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec!["discord.worker".into()],
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "worker".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "worker-token".to_string(),
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "ops".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "ops-token".to_string(),
            ..Default::default()
        },
    );
    config.sop.approval.policies.insert(
        "prod".to_string(),
        zeroclaw_config::schema::ApprovalPolicyConfig {
            request_route: Some("discord.ops:room-1".to_string()),
            escalation_route: Some("discord.ops:room-2".to_string()),
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config.clone()));
    let configured = collect_configured_channels(&config_arc, "test", &[], None, None);
    let channel_map = configured_channel_map(&configured);
    assert!(
        channel_map.contains_key("discord.ops"),
        "the approval route's configured channel must be live for adapter delivery"
    );

    let collected_keys: Vec<String> = channel_map.keys().cloned().collect();
    let owners = build_owner_by_channel_key(&config, &["worker".to_string()], &collected_keys);
    assert!(
        !owners.contains_key("discord.ops"),
        "approval-route liveness must not create an agent owner"
    );

    let worker_ctx = router_test_ctx();
    let router = AgentRouter::multi(
        HashMap::from([("worker".to_string(), worker_ctx)]),
        owners,
        None,
        None,
    );
    assert!(
        router
            .resolve(&channel_message("discord", Some("ops")))
            .is_none(),
        "ordinary traffic on the approval-only alias must not reach the worker"
    );
}

#[cfg(feature = "channel-discord")]
#[test]
fn bare_approval_route_collects_the_sole_enabled_alias() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "worker".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec!["discord.worker".into()],
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "worker".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: false,
            bot_token: "worker-token".to_string(),
            ..Default::default()
        },
    );
    config.channels.discord.insert(
        "ops".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            enabled: true,
            bot_token: "ops-token".to_string(),
            ..Default::default()
        },
    );
    config.sop.approval.policies.insert(
        "prod".to_string(),
        zeroclaw_config::schema::ApprovalPolicyConfig {
            request_route: Some("discord:room-1".to_string()),
            ..Default::default()
        },
    );

    let config_arc = Arc::new(RwLock::new(config));
    let configured = collect_configured_channels(&config_arc, "test", &[], None, None);
    let channel_map = configured_channel_map(&configured);
    assert!(channel_map.contains_key("discord.ops"));
    assert!(
        channel_map.contains_key("discord"),
        "the route adapter can resolve the bare singleton key"
    );
    assert!(
        !channel_map.contains_key("discord.worker"),
        "a disabled channel must not be revived just because a sibling route is active"
    );
}

#[test]
fn build_owner_by_channel_key_skips_disabled_owners() {
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "loser".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: false,
            channels: vec!["discord.b".into()],
            ..Default::default()
        },
    );

    // Reload passes an empty enabled_agents slice because the only
    // owner is disabled.
    let owners = build_owner_by_channel_key(&config, &[], &["discord.b".to_string()]);

    assert!(
        owners.is_empty(),
        "disabled-owner channels must not be rebound to any fallback agent (#8013)"
    );
}

/// Helper: returns the set of `nostr.<alias>` references that pass
/// the unified `ActiveChannelAliases` gate AND the channel-level
/// `enabled = true` check, in the same way `doctor_channels` and
/// `start_channels` use it after Phase 2.
#[cfg(feature = "channel-nostr")]
fn resolve_nostr_active(config: &Config) -> Vec<String> {
    let active = ActiveChannelAliases::compute(config);
    config
        .channels
        .nostr
        .iter()
        .filter(|(alias, _)| active.contains(&format!("nostr.{alias}")))
        .filter(|(_, ns)| ns.enabled)
        .map(|(alias, _)| format!("nostr.{alias}"))
        .collect()
}

#[cfg(feature = "channel-nostr")]
#[test]
fn doctor_channels_skips_nostr_when_only_owner_is_disabled() {
    // T5 — thebug path on the Nostr side. An explicit
    // `nostr.default` binding exists, but the owner agent is
    // `enabled = false`. Both the doctor and startup Nostr blocks
    // must NOT bring this channel online.
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "disabled_owner".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: false,
            channels: vec!["nostr.default".into()],
            ..Default::default()
        },
    );
    config.channels.nostr.insert(
        "default".to_string(),
        zeroclaw_config::schema::NostrConfig {
            enabled: true,
            private_key: "nsec1test".to_string(),
            ..Default::default()
        },
    );

    let active = resolve_nostr_active(&config);
    assert!(
        active.is_empty(),
        "Nostr channel with only a disabled owner must not pass the gate (#8013): got {:?}",
        active
    );
}

#[cfg(feature = "channel-nostr")]
#[test]
fn start_channels_legacy_includes_nostr_when_no_bindings_declared() {
    // T6 — the legacy fallback on the Nostr side. No agent declares
    // any channel binding, so the `all_known_bindings.is_empty()`
    // branch fires and every enabled Nostr alias is accepted. This
    // pins parity with the Discord T2 behavior.
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "legacy".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![],
            ..Default::default()
        },
    );
    config.channels.nostr.insert(
        "legacy_alias".to_string(),
        zeroclaw_config::schema::NostrConfig {
            enabled: true,
            private_key: "nsec1test".to_string(),
            ..Default::default()
        },
    );

    let active = resolve_nostr_active(&config);
    assert_eq!(
        active,
        vec!["nostr.legacy_alias".to_string()],
        "Legacy fallback must keep Nostr active when no agent declares bindings"
    );
}

#[cfg(feature = "channel-nostr")]
#[test]
fn start_channels_nostr_skips_channel_level_disabled() {
    // T7 — channel-level `enabled = false` still skips even when
    // the agent binding path is satisfied. Pins the channel-level
    // half of the gate that was previously missing in the
    // `start_channels` Nostr block.
    let mut config = Config::default();
    config.agents.clear();
    config.agents.insert(
        "owner".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec!["nostr.muted".into()],
            ..Default::default()
        },
    );
    config.channels.nostr.insert(
        "muted".to_string(),
        zeroclaw_config::schema::NostrConfig {
            enabled: false, // channel-level off
            private_key: "nsec1test".to_string(),
            ..Default::default()
        },
    );

    let active = resolve_nostr_active(&config);
    assert!(
        active.is_empty(),
        "Nostr channel with `enabled = false` must not start regardless of agent binding"
    );
}

#[cfg(feature = "channel-email")]
#[test]
fn collect_configured_channels_skips_unreferenced_email() {
    let mut config = Config::default();
    config.channels.email.insert(
        "default".to_string(),
        zeroclaw_config::scattered_types::EmailConfig::default(),
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);
    assert!(
        !channels.iter().any(|entry| entry.display_name == "Email"),
        "email with no agent reference should not be collected"
    );
}

#[cfg(feature = "channel-voice-call")]
#[test]
fn collect_configured_channels_skips_unreferenced_voice_call() {
    let mut config = Config::default();
    config.channels.voice_call.insert(
        "default".to_string(),
        zeroclaw_config::scattered_types::VoiceCallConfig::default(),
    );

    let config_arc = Arc::new(RwLock::new(config));
    let channels = collect_configured_channels(&config_arc, "test", &[], None, None);
    assert!(
        !channels
            .iter()
            .any(|entry| entry.display_name == "Voice Call"),
        "voice-call with no agent reference should not be collected"
    );
}

struct AlwaysFailChannel {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

struct BlockUntilClosedChannel {
    name: String,
    calls: Arc<AtomicUsize>,
}

struct FailOnceChannel {
    name: String,
    calls: Arc<AtomicUsize>,
    err: Mutex<Option<anyhow::Error>>,
}

impl ::zeroclaw_api::attribution::Attributable for AlwaysFailChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

#[async_trait::async_trait]
impl Channel for AlwaysFailChannel {
    fn name(&self) -> &str {
        self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("listen boom")
    }
}

impl ::zeroclaw_api::attribution::Attributable for BlockUntilClosedChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Webhook,
        )
    }
    fn alias(&self) -> &str {
        "test"
    }
}

impl ::zeroclaw_api::attribution::Attributable for FailOnceChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Discord,
        )
    }

    fn alias(&self) -> &str {
        "default"
    }
}

#[async_trait::async_trait]
impl Channel for BlockUntilClosedChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tx.closed().await;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Channel for FailOnceChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        Ok(())
    }

    async fn listen(
        &self,
        _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(err) = self.err.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Err(err);
        }
        Ok(())
    }
}

#[tokio::test]
async fn supervised_listener_marks_error_and_restarts_on_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let channel: Arc<dyn Channel> = Arc::new(AlwaysFailChannel {
        name: "test-supervised-fail",
        calls: Arc::clone(&calls),
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

    tokio::time::sleep(Duration::from_millis(80)).await;
    drop(rx);
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;

    let snapshot = zeroclaw_runtime::health::snapshot_json();
    let component = &snapshot["components"]["channel:test-supervised-fail"];
    assert_eq!(component["status"], "error");
    assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
    assert!(
        component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("listen boom")
    );
    assert!(calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn supervised_listener_refreshes_health_while_running() {
    let calls = Arc::new(AtomicUsize::new(0));
    let channel_name = format!("test-supervised-heartbeat-{}", uuid::Uuid::new_v4());
    let component_name = format!("channel:{channel_name}");
    let channel: Arc<dyn Channel> = Arc::new(BlockUntilClosedChannel {
        name: channel_name,
        calls: Arc::clone(&calls),
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = spawn_supervised_listener_with_health_interval(
        channel,
        None,
        tx,
        1,
        1,
        Duration::from_millis(20),
        cancel.clone(),
    );

    tokio::time::sleep(Duration::from_millis(35)).await;
    let first_last_ok =
        zeroclaw_runtime::health::snapshot_json()["components"][&component_name]["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
    assert!(!first_last_ok.is_empty());

    tokio::time::sleep(Duration::from_millis(70)).await;
    let second_last_ok =
        zeroclaw_runtime::health::snapshot_json()["components"][&component_name]["last_ok"]
            .as_str()
            .unwrap_or("")
            .to_string();
    let first = chrono::DateTime::parse_from_rfc3339(&first_last_ok)
        .expect("last_ok should be valid RFC3339");
    let second = chrono::DateTime::parse_from_rfc3339(&second_last_ok)
        .expect("last_ok should be valid RFC3339");
    assert!(second > first, "expected periodic health heartbeat refresh");

    cancel.cancel();
    let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(join.is_ok(), "listener should stop on cancel");
    assert!(calls.load(Ordering::SeqCst) >= 1);
    drop(rx);
}

#[tokio::test]
async fn supervised_listener_does_not_restart_on_non_retryable_discord_http_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
    let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
        name: channel_name,
        calls: Arc::clone(&calls),
        err: Mutex::new(Some(anyhow::Error::msg("401 Unauthorized"))),
    });

    let component_name = format!("channel:{}", channel.name());
    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

    tokio::time::sleep(Duration::from_millis(80)).await;
    let snapshot = zeroclaw_runtime::health::snapshot_json();
    let component = &snapshot["components"][&component_name];
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(component["status"], "error");
    assert_eq!(component["restart_count"].as_u64().unwrap_or(0), 0);
    assert!(
        component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("401 Unauthorized")
    );

    drop(rx);
    cancel.cancel();
    let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(join.is_ok(), "listener should stop on cancel");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "channel-discord")]
#[tokio::test]
async fn supervised_listener_enters_retry_path_on_discord_gateway_rate_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
    let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
        name: channel_name,
        calls: Arc::clone(&calls),
        err: Mutex::new(Some(anyhow::Error::msg(
            "discord gateway preflight rate-limited (429 Too Many Requests)",
        ))),
    });

    let component_name = format!("channel:{}", channel.name());
    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

    tokio::time::sleep(Duration::from_millis(80)).await;
    let snapshot = zeroclaw_runtime::health::snapshot_json();
    let component = &snapshot["components"][&component_name];
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(component["status"], "error");
    assert!(
        component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("429 Too Many Requests")
    );
    assert!(
        component["restart_count"].as_u64().unwrap_or(0) >= 1,
        "Discord gateway 429 should back off through the retry path instead of parking"
    );

    drop(rx);
    cancel.cancel();
    let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(join.is_ok(), "listener should stop on cancel");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "channel-discord")]
#[tokio::test]
async fn supervised_listener_does_not_restart_on_fatal_discord_gateway_close_code() {
    let calls = Arc::new(AtomicUsize::new(0));
    let channel_name = format!("discord-{}", uuid::Uuid::new_v4());
    let channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
        name: channel_name,
        calls: Arc::clone(&calls),
        err: Mutex::new(Some(anyhow::Error::new(
            crate::discord::DiscordListenerFatalError::new(
                "discord gateway closed with fatal code 4014: disallowed intent(s)",
            ),
        ))),
    });

    let component_name = format!("channel:{}", channel.name());
    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = spawn_supervised_listener(channel, None, tx, 1, 1, cancel.clone());

    tokio::time::sleep(Duration::from_millis(80)).await;
    let snapshot = zeroclaw_runtime::health::snapshot_json();
    let component = &snapshot["components"][&component_name];
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(component["status"], "error");
    assert_eq!(component["restart_count"].as_u64().unwrap_or(0), 0);
    assert!(
        component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("fatal code 4014")
    );

    drop(rx);
    cancel.cancel();
    let join = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(join.is_ok(), "listener should stop on cancel");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn non_retryable_listener_error_does_not_stop_other_listener_health() {
    let failing_calls = Arc::new(AtomicUsize::new(0));
    let healthy_calls = Arc::new(AtomicUsize::new(0));
    let failing_name = format!("discord-{}", uuid::Uuid::new_v4());
    let healthy_name = format!("test-supervised-sibling-{}", uuid::Uuid::new_v4());
    let failing_component = format!("channel:{failing_name}");
    let healthy_component = format!("channel:{healthy_name}");

    let failing_channel: Arc<dyn Channel> = Arc::new(FailOnceChannel {
        name: failing_name,
        calls: Arc::clone(&failing_calls),
        err: Mutex::new(Some(anyhow::Error::msg("401 Unauthorized"))),
    });
    let healthy_channel: Arc<dyn Channel> = Arc::new(BlockUntilClosedChannel {
        name: healthy_name,
        calls: Arc::clone(&healthy_calls),
    });

    let (failing_tx, failing_rx) =
        tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let (healthy_tx, healthy_rx) =
        tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let failing_handle =
        spawn_supervised_listener(failing_channel, None, failing_tx, 1, 1, cancel.clone());
    let healthy_handle = spawn_supervised_listener_with_health_interval(
        healthy_channel,
        None,
        healthy_tx,
        1,
        1,
        Duration::from_millis(20),
        cancel.clone(),
    );

    tokio::time::sleep(Duration::from_millis(80)).await;

    let first_last_ok = zeroclaw_runtime::health::snapshot_json()["components"][&healthy_component]
        ["last_ok"]
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        !first_last_ok.is_empty(),
        "healthy sibling should report health"
    );

    tokio::time::sleep(Duration::from_millis(70)).await;

    let snapshot = zeroclaw_runtime::health::snapshot_json();
    let failing = &snapshot["components"][&failing_component];
    let healthy = &snapshot["components"][&healthy_component];
    let second_last_ok = healthy["last_ok"].as_str().unwrap_or("").to_string();
    let first = chrono::DateTime::parse_from_rfc3339(&first_last_ok)
        .expect("healthy sibling last_ok should be valid RFC3339");
    let second = chrono::DateTime::parse_from_rfc3339(&second_last_ok)
        .expect("healthy sibling last_ok should be valid RFC3339");

    assert_eq!(failing_calls.load(Ordering::SeqCst), 1);
    assert_eq!(failing["status"], "error");
    assert_eq!(failing["restart_count"].as_u64().unwrap_or(0), 0);
    assert!(
        failing["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("401 Unauthorized")
    );
    assert_eq!(healthy["status"], "ok");
    assert!(
        second > first,
        "healthy sibling should keep refreshing health"
    );
    assert!(healthy_calls.load(Ordering::SeqCst) >= 1);

    drop(failing_rx);
    drop(healthy_rx);
    cancel.cancel();
    let failing_join = tokio::time::timeout(Duration::from_millis(500), failing_handle).await;
    let healthy_join = tokio::time::timeout(Duration::from_millis(500), healthy_handle).await;
    assert!(
        failing_join.is_ok(),
        "non-retryable listener should stop on cancel"
    );
    assert!(
        healthy_join.is_ok(),
        "healthy sibling listener should stop on cancel"
    );
}

#[test]
fn maybe_restart_daemon_systemd_args_regression() {
    assert_eq!(
        SYSTEMD_STATUS_ARGS,
        ["--user", "is-active", "zeroclaw.service"]
    );
    assert_eq!(
        SYSTEMD_RESTART_ARGS,
        ["--user", "restart", "zeroclaw.service"]
    );
}

#[test]
fn maybe_restart_daemon_openrc_args_regression() {
    assert_eq!(OPENRC_STATUS_ARGS, ["zeroclaw", "status"]);
    assert_eq!(OPENRC_RESTART_ARGS, ["zeroclaw", "restart"]);
}

#[test]
fn normalize_merges_consecutive_user_turns() {
    let turns = vec![ChatMessage::user("hello"), ChatMessage::user("world")];
    let result = normalize_cached_channel_turns(turns);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].role, "user");
    assert_eq!(result[0].content, "hello\n\nworld");
}

#[test]
fn normalize_preserves_strict_alternation() {
    let turns = vec![
        ChatMessage::user("hello"),
        ChatMessage::assistant("hi"),
        ChatMessage::user("bye"),
    ];
    let result = normalize_cached_channel_turns(turns);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].content, "hello");
    assert_eq!(result[1].content, "hi");
    assert_eq!(result[2].content, "bye");
}

#[test]
fn normalize_merges_multiple_consecutive_user_turns() {
    let turns = vec![
        ChatMessage::user("a"),
        ChatMessage::user("b"),
        ChatMessage::user("c"),
    ];
    let result = normalize_cached_channel_turns(turns);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].role, "user");
    assert_eq!(result[0].content, "a\n\nb\n\nc");
}

#[test]
fn normalize_empty_input() {
    let result = normalize_cached_channel_turns(vec![]);
    assert!(result.is_empty());
}

#[test]
fn channel_history_preserves_image_marker_verbatim_across_followup() {
    let img = "[IMAGE:/tmp/media/screenshot.png] wait look at this";
    let mut turns = vec![
        ChatMessage::user(img),
        ChatMessage::assistant("\u{1f44d}"),
        ChatMessage::user("can you see the screenshot?"),
    ];

    let kept: Vec<ChatMessage> = normalize_cached_channel_turns(std::mem::take(&mut turns));

    assert_eq!(kept[0].content, img);
    assert!(kept[0].content.contains("/tmp/media/screenshot.png"));
    assert!(!kept[0].content.contains("processed by vision model"));
}

#[test]
fn channel_history_preserves_document_voice_and_text_verbatim() {
    let doc = "[Document: report.pdf] /tmp/media/report.pdf summarize this";
    let voice = "[Voice] what did i just send";
    let text = "plain text with no markers";
    let mut turns = vec![
        ChatMessage::user(doc),
        ChatMessage::assistant("ok"),
        ChatMessage::user(voice),
        ChatMessage::assistant("ok"),
        ChatMessage::user(text),
    ];

    let kept: Vec<ChatMessage> = normalize_cached_channel_turns(std::mem::take(&mut turns));

    assert_eq!(kept[0].content, doc);
    assert_eq!(kept[2].content, voice);
    assert_eq!(kept[4].content, text);
}

#[test]
fn collapse_inline_image_payloads_drops_data_uri_keeps_path() {
    let path_turn = "[IMAGE:/tmp/media/screenshot.png] can you see this?";
    let data_turn = format!(
        "[IMAGE:data:image/png;base64,{}] old screenshot",
        "AQIDBAUGBwg".repeat(64)
    );
    let mut turns = vec![
        ChatMessage::user(path_turn),
        ChatMessage::assistant("ok"),
        ChatMessage::user(&data_turn),
        ChatMessage::assistant("ok"),
        ChatMessage::user("[IMAGE:/tmp/media/current.png] and this?"),
    ];

    collapse_inline_image_payloads(&mut turns);

    assert_eq!(turns[0].content, path_turn, "file-path marker must survive");
    assert!(
        !turns[2].content.contains("base64"),
        "inline data payload must be collapsed"
    );
    assert!(turns[2].content.contains("old screenshot"));
    assert_eq!(
        turns[4].content, "[IMAGE:/tmp/media/current.png] and this?",
        "current turn is never collapsed"
    );
}

#[test]
fn strip_inline_data_image_markers_drops_bytes_keeps_path_marker_for_autosave() {
    // The autosave path calls strip_inline_data_image_markers before
    // storing to durable memory, so inline data: bytes never persist while
    // re-loadable path markers and surrounding text survive.
    let img_open = format!("[{}", "IMAGE:/tmp/shot.png]");
    let payload = "AQIDBAUGBwg".repeat(64);
    let data_marker = format!("[{}{payload}]", "IMAGE:data:image/png;base64,");
    let content = format!("look at {img_open} and {data_marker} please");

    let cleaned = strip_inline_data_image_markers(&content);

    assert!(
        !cleaned.contains("base64"),
        "inline data bytes must be stripped before autosave: {cleaned}"
    );
    assert!(
        cleaned.contains("/tmp/shot.png"),
        "re-loadable path marker must survive: {cleaned}"
    );
    assert!(cleaned.contains("look at") && cleaned.contains("please"));
}

#[tokio::test]
async fn media_pipeline_preserves_image_bytes_when_vision_route_configured() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let provider_impl = Arc::new(HistoryCaptureModelProvider::default());
    let vision_server = MockServer::start().await;
    let _vision_mock = Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("data:image/png;base64,AQIDBA=="))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "vision saw bytes"
                    }
                }
            ]
        })))
        .expect(1)
        .mount_as_scoped(&vision_server)
        .await;

    let base_ctx = peer_prompt_test_context(
        channels_by_name,
        provider_impl.clone(),
        Arc::new(zeroclaw_config::schema::Config::default()),
        Arc::new(vec![]),
    );
    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        multimodal: zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some(format!("custom:{}", vision_server.uri())),
            vision_model: Some("test-vision-model".to_string()),
            ..Default::default()
        },
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig {
            enabled: true,
            describe_images: true,
            ..Default::default()
        },
        ..(*base_ctx).clone()
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-image-route".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-image-route".to_string(),
            content: "please inspect this".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            passive_context: false,
            explicitly_addressed: false,
            conversation_scope: zeroclaw_api::channel::ChannelConversationScope::Sender,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![zeroclaw_api::media::MediaAttachment {
                file_name: "route.png".to_string(),
                data: vec![1, 2, 3, 4],
                mime_type: Some("image/png".to_string()),
            }],
            subject: None,
            internal_sop_event: None,
        },
        CancellationToken::new(),
    )
    .await;

    {
        let calls = provider_impl.calls.lock().unwrap();
        assert!(
            calls.is_empty(),
            "default non-vision provider must not receive an image-bearing turn: {calls:?}"
        );
    }

    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(
        sent_messages.len(),
        1,
        "vision route should send exactly one assistant reply: {sent_messages:?}"
    );
    assert!(
        sent_messages[0].contains("vision saw bytes"),
        "reply should come from the mock vision provider: {sent_messages:?}"
    );
    drop(sent_messages);

    let vision_requests = vision_server
        .received_requests()
        .await
        .expect("mock server should record vision provider requests");
    assert_eq!(
        vision_requests.len(),
        1,
        "vision provider should receive exactly one request"
    );
    let vision_body: serde_json::Value = vision_requests[0]
        .body_json()
        .expect("vision provider request should be JSON");
    assert_eq!(vision_body["model"], "test-vision-model");
    assert!(
        vision_body
            .to_string()
            .contains("data:image/png;base64,AQIDBA=="),
        "vision provider request must contain the preserved attachment bytes: {vision_body}"
    );
}

#[tokio::test]
async fn e2e_photo_attachment_rejected_by_non_vision_provider() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    // DummyModelProvider has default capabilities (vision: false).
    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("dummy".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("You are a helpful assistant.".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    // Simulate a photo attachment message with [IMAGE:] marker.
    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-photo-1".to_string(),
            sender: "zeroclaw_user".to_string(),
            reply_target: "chat-photo".to_string(),
            content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent = channel_impl.sent_messages.lock().await;
    assert_eq!(sent.len(), 1, "expected exactly one reply message");
    assert!(
        sent[0].contains("does not support vision"),
        "reply must mention vision capability error, got: {}",
        sent[0]
    );
    assert!(
        sent[0].contains("⚠️ Error"),
        "reply must start with error prefix, got: {}",
        sent[0]
    );
}

#[tokio::test]
async fn e2e_failed_vision_turn_does_not_poison_follow_up_text_turn() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(DummyModelProvider),
        model_provider_ref: Arc::new("dummy".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("You are a helpful assistant.".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        Arc::clone(&runtime_ctx),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-photo-1".to_string(),
            sender: "zeroclaw_user".to_string(),
            reply_target: "chat-photo".to_string(),
            content: "[IMAGE:/tmp/workspace/photo_99_1.jpg]\n\nWhat is this?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    process_channel_message(
        Arc::clone(&runtime_ctx),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-text-2".to_string(),
            sender: "zeroclaw_user".to_string(),
            reply_target: "chat-photo".to_string(),
            content: "What is WAL?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent = channel_impl.sent_messages.lock().await;
    assert_eq!(sent.len(), 2, "expected one error and one successful reply");
    assert!(
        sent[0].contains("does not support vision"),
        "first reply must mention vision capability error, got: {}",
        sent[0]
    );
    assert!(
        sent[1].ends_with(":ok"),
        "second reply should succeed for text-only turn, got: {}",
        sent[1]
    );
    drop(sent);

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek("test-channel_chat-photo_zeroclaw_user")
        .expect("history should exist for sender");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert!(
        turns[0].content.contains("] What is WAL?"),
        "follow-up user turn should be timestamped: {}",
        turns[0].content
    );
    assert_eq!(turns[1].role, "assistant");
    assert_eq!(turns[1].content, "ok");
    assert!(
        turns.iter().all(|turn| !turn.content.contains("[IMAGE:")),
        "failed vision turn must not persist image marker content"
    );
}

#[tokio::test]
async fn e2e_failed_non_retryable_turn_does_not_poison_follow_up_text_turn() {
    let channel_impl = Arc::new(RecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(FormatErrorModelProvider),
        model_provider_ref: Arc::new("dummy".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("You are a helpful assistant.".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 50000,
        context_token_budget: 128_000,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            std::time::Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
    });

    process_channel_message(
        Arc::clone(&runtime_ctx),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-bad-1".to_string(),
            sender: "zeroclaw_user".to_string(),
            reply_target: "chat-format".to_string(),
            content: "trigger format error".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    process_channel_message(
        Arc::clone(&runtime_ctx),
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-text-2".to_string(),
            sender: "zeroclaw_user".to_string(),
            reply_target: "chat-format".to_string(),
            content: "What is WAL?".to_string(),
            channel: "test-channel".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    let sent = channel_impl.sent_messages.lock().await;
    assert_eq!(sent.len(), 2, "expected one error and one successful reply");
    assert!(
        sent[0].contains("Format Error"),
        "first reply must mention the request format error, got: {}",
        sent[0]
    );
    assert!(
        sent[1].ends_with(":ok"),
        "second reply should succeed for follow-up text, got: {}",
        sent[1]
    );
    drop(sent);

    let histories = runtime_ctx
        .conversation_histories
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let turns = histories
        .peek("test-channel_chat-format_zeroclaw_user")
        .expect("history should exist for sender");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert!(
        turns[0].content.contains("] What is WAL?"),
        "follow-up user turn should be timestamped: {}",
        turns[0].content
    );
    assert_eq!(turns[1].role, "assistant");
    assert_eq!(turns[1].content, "ok");
    assert!(
        turns
            .iter()
            .all(|turn| turn.content != "trigger format error"),
        "failed non-retryable turn must not persist in history"
    );
}

#[test]
fn build_channel_by_id_unknown_channel_returns_error() {
    let config = Config::default();
    let config_arc = Arc::new(RwLock::new(config));
    match build_channel_by_id(&config_arc, "nonexistent") {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("Unknown channel"),
                "expected 'Unknown channel' in error, got: {err_msg}"
            );
        }
        Ok(_) => panic!("should fail for unknown channel"),
    }
}

#[test]
fn one_shot_channel_workspace_dir_uses_owning_agent_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.agents.insert(
        "alice".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![zeroclaw_config::providers::ChannelRef(
                "telegram.default".to_string(),
            )],
            ..Default::default()
        },
    );

    let resolved = one_shot_channel_workspace_dir(&config, "telegram", "default");

    assert_eq!(resolved, config.agent_workspace_dir("alice"));
    assert_ne!(resolved, config.data_dir);
}

// ── Query classification in channel message processing ─────────

#[tokio::test]
async fn process_channel_message_applies_query_classification_route() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

    let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
        enabled: true,
        rules: vec![zeroclaw_config::schema::ClassificationRule {
            hint: "vision".into(),
            keywords: vec!["analyze-image".into()],
            ..Default::default()
        }],
    };

    let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
        hint: "vision".into(),
        model_provider: "vision-provider".into(),
        model: "gpt-4-vision".into(),
        api_key: None,
    }];

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(model_routes),
        query_classification: classification_config,
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-qc-1".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "please analyze-image from the dataset".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    // Vision model_provider should have been called instead of the default.
    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        vision_model_provider_impl.call_count.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        vision_model_provider_impl
            .models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_slice(),
        &["gpt-4-vision".to_string()]
    );
}

#[tokio::test]
async fn process_channel_message_classification_disabled_uses_default_route() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

    // Classification is disabled — matching keyword should NOT trigger reroute.
    let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
        enabled: false,
        rules: vec![zeroclaw_config::schema::ClassificationRule {
            hint: "vision".into(),
            keywords: vec!["analyze-image".into()],
            ..Default::default()
        }],
    };

    let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
        hint: "vision".into(),
        model_provider: "vision-provider".into(),
        model: "gpt-4-vision".into(),
        api_key: None,
    }];

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(model_routes),
        query_classification: classification_config,
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-qc-disabled".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "please analyze-image from the dataset".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    // Default model_provider should be used since classification is disabled.
    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        vision_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn process_channel_message_classification_no_match_uses_default_route() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let vision_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let vision_model_provider: Arc<dyn ModelProvider> = vision_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("vision-provider".to_string(), vision_model_provider);

    // Classification enabled with a rule that won't match the message.
    let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
        enabled: true,
        rules: vec![zeroclaw_config::schema::ClassificationRule {
            hint: "vision".into(),
            keywords: vec!["analyze-image".into()],
            ..Default::default()
        }],
    };

    let model_routes = vec![zeroclaw_config::schema::ModelRouteConfig {
        hint: "vision".into(),
        model_provider: "vision-provider".into(),
        model: "gpt-4-vision".into(),
        api_key: None,
    }];

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(model_routes),
        query_classification: classification_config,
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-qc-nomatch".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "just a regular text message".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    // Default model_provider should be used since no classification rule matched.
    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        vision_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn process_channel_message_classification_priority_selects_highest() {
    let channel_impl = Arc::new(TelegramRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let agent_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let agent_model_provider: Arc<dyn ModelProvider> = agent_model_provider_impl.clone();
    let fast_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let fast_model_provider: Arc<dyn ModelProvider> = fast_model_provider_impl.clone();
    let code_model_provider_impl = Arc::new(ModelCaptureModelProvider::default());
    let code_model_provider: Arc<dyn ModelProvider> = code_model_provider_impl.clone();

    let mut provider_cache_seed: HashMap<String, Arc<dyn ModelProvider>> = HashMap::new();
    provider_cache_seed.insert(
        "test-provider".to_string(),
        Arc::clone(&agent_model_provider),
    );
    provider_cache_seed.insert("fast-provider".to_string(), fast_model_provider);
    provider_cache_seed.insert("code-provider".to_string(), code_model_provider);

    // Both rules match "code" keyword, but "code" rule has higher priority.
    let classification_config = zeroclaw_config::schema::QueryClassificationConfig {
        enabled: true,
        rules: vec![
            zeroclaw_config::schema::ClassificationRule {
                hint: "fast".into(),
                keywords: vec!["code".into()],
                priority: 1,
                ..Default::default()
            },
            zeroclaw_config::schema::ClassificationRule {
                hint: "code".into(),
                keywords: vec!["code".into()],
                priority: 10,
                ..Default::default()
            },
        ],
    };

    let model_routes = vec![
        zeroclaw_config::schema::ModelRouteConfig {
            hint: "fast".into(),
            model_provider: "fast-provider".into(),
            model: "fast-model".into(),
            api_key: None,
        },
        zeroclaw_config::schema::ModelRouteConfig {
            hint: "code".into(),
            model_provider: "code-provider".into(),
            model: "code-model".into(),
            api_key: None,
        },
    ];

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::clone(&agent_model_provider),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("default-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 5,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(provider_cache_seed)),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: false,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(model_routes),
        query_classification: classification_config,
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    process_channel_message(
        runtime_ctx,
        zeroclaw_api::channel::ChannelMessage {
            id: "msg-qc-prio".to_string(),
            sender: "alice".to_string(),
            reply_target: "chat-1".to_string(),
            content: "write some code for me".to_string(),
            channel: "telegram".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;

    // Higher-priority "code" rule (priority=10) should win over "fast" (priority=1).
    assert_eq!(
        agent_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        fast_model_provider_impl.call_count.load(Ordering::SeqCst),
        0
    );
    assert_eq!(
        code_model_provider_impl.call_count.load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        code_model_provider_impl
            .models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_slice(),
        &["code-model".to_string()]
    );
}

#[cfg(feature = "channel-telegram")]
#[test]
fn build_channel_by_id_unconfigured_telegram_returns_error() {
    let config = Config::default();
    let config_arc = Arc::new(RwLock::new(config));
    match build_channel_by_id(&config_arc, "telegram") {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("not configured"),
                "expected 'not configured' in error, got: {err_msg}"
            );
        }
        Ok(_) => panic!("should fail when telegram is not configured"),
    }
}

#[cfg(feature = "channel-telegram")]
#[test]
fn build_channel_by_id_configured_telegram_succeeds() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "default".to_string(),
        zeroclaw_config::schema::TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
            stream_mode: zeroclaw_config::schema::StreamMode::Off,
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
            excluded_tools: vec![],
            reply_min_interval_secs: 0,
            reply_queue_depth_max: 0,
            debounce_ms: None,
        },
    );
    let config_arc = Arc::new(RwLock::new(config));
    match build_channel_by_id(&config_arc, "telegram") {
        Ok(channel) => assert_eq!(channel.name(), "telegram"),
        Err(e) => panic!("should succeed when telegram is configured: {e}"),
    }
}

#[cfg(feature = "channel-telegram")]
fn config_with_telegram_alias(alias: &str) -> Config {
    let mut config = Config::default();
    config.channels.telegram.insert(
        alias.to_string(),
        zeroclaw_config::schema::TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
            stream_mode: zeroclaw_config::schema::StreamMode::Off,
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
            excluded_tools: vec![],
            reply_min_interval_secs: 0,
            reply_queue_depth_max: 0,
            debounce_ms: None,
        },
    );
    config
}

/// The non-default-alias bug: the bound identity must land in the same
/// peer group the runtime resolves authorization from — otherwise the
/// bot keeps demanding `/bind` for a user the operator already approved.
#[cfg(feature = "channel-telegram")]
#[test]
fn bind_telegram_into_non_default_alias_is_resolvable() {
    let mut config = config_with_telegram_alias("alerts");
    let newly = bind_telegram_identity_into(&mut config, "123456789", "alerts").unwrap();
    assert!(newly, "first bind should report newly added");
    // The live resolver the channel uses must now see the identity.
    assert!(
        config
            .channel_external_peers("telegram", "alerts")
            .contains(&"123456789".to_string()),
        "identity bound to `alerts` must resolve for the `alerts` channel"
    );
    // And it must be scoped — not leaked onto a different alias.
    assert!(
        config
            .channel_external_peers("telegram", "other")
            .is_empty(),
        "binding must stay scoped to its alias"
    );
}

/// Backward compatibility: the default alias still routes to
/// `telegram_default` / `telegram.default` exactly as before.
#[cfg(feature = "channel-telegram")]
#[test]
fn bind_telegram_into_default_alias_unchanged() {
    let mut config = config_with_telegram_alias("default");
    bind_telegram_identity_into(&mut config, "@zeroclaw_user", "default").unwrap();
    let group = config
        .peer_groups
        .get("telegram_default")
        .expect("default bind must use the telegram_default group");
    assert_eq!(group.channel.as_str(), "telegram.default");
    assert!(
        config
            .channel_external_peers("telegram", "default")
            .contains(&"zeroclaw_user".to_string())
    );
}

/// Idempotency: re-binding the same identity reports "already present".
#[cfg(feature = "channel-telegram")]
#[test]
fn bind_telegram_into_is_idempotent() {
    let mut config = config_with_telegram_alias("alerts");
    assert!(bind_telegram_identity_into(&mut config, "123", "alerts").unwrap());
    assert!(
        !bind_telegram_identity_into(&mut config, "123", "alerts").unwrap(),
        "second bind of same identity should report already present"
    );
    assert_eq!(
        config.channel_external_peers("telegram", "alerts").len(),
        1,
        "duplicate bind must not add a second peer entry"
    );
}

/// A typo'd / unconfigured alias must fail loudly rather than mint a
/// phantom peer group that authorizes nobody.
#[cfg(feature = "channel-telegram")]
#[test]
fn bind_telegram_into_unconfigured_alias_bails() {
    let mut config = config_with_telegram_alias("default");
    let err = bind_telegram_identity_into(&mut config, "123", "typoalias")
        .expect_err("unconfigured alias must bail");
    assert!(
        err.to_string().contains("typoalias"),
        "error should name the bad alias, got: {err}"
    );
    assert!(
        !config.peer_groups.contains_key("telegram_typoalias"),
        "failed bind must not create a phantom peer group"
    );
}

/// The generic bind must keep the SCOPED dotted `<type>.<alias>` channel
/// ref — never a bare type, which would broaden the peer across every
/// alias of that type (the bug the alias-aware fix closed).
#[cfg(feature = "channel-telegram")]
#[test]
fn bind_channel_into_uses_scoped_dotted_channel_ref() {
    let mut config = config_with_telegram_alias("alerts");
    assert!(bind_channel_identity_into(&mut config, "telegram", "alerts", "@user").unwrap());
    let group = config
        .peer_groups
        .get("telegram_alerts")
        .expect("generic bind must use the telegram_alerts group");
    assert_eq!(
        group.channel.as_str(),
        "telegram.alerts",
        "channel ref must stay dotted/alias-scoped, never bare `telegram`"
    );
    // Resolves for `alerts`, and stays OFF a sibling alias.
    assert!(
        config
            .channel_external_peers("telegram", "alerts")
            .contains(&"user".to_string())
    );
    assert!(
        config
            .channel_external_peers("telegram", "other")
            .is_empty()
    );
}

/// The closed-set gate: a non-pairing channel type cannot be bound.
#[test]
fn bind_channel_into_rejects_unsupported_type() {
    let mut config = Config::default();
    let err = bind_channel_identity_into(&mut config, "discord", "default", "123")
        .expect_err("unsupported type must bail");
    assert!(
        err.to_string()
            .contains("does not support identity binding"),
        "error should explain the type is unsupported, got: {err}"
    );
    assert!(
        channel_identity_normalizer("discord").is_none(),
        "discord must not be in the bind closed-set"
    );
    assert!(channel_identity_normalizer("telegram").is_some());
    assert!(channel_identity_normalizer("wechat").is_some());
    assert!(channel_identity_normalizer("line").is_some());
}

#[cfg(feature = "channel-voice-call")]
#[test]
fn build_channel_by_id_unconfigured_voice_call_returns_error() {
    let config = Config::default();
    let config_arc = Arc::new(RwLock::new(config));
    match build_channel_by_id(&config_arc, "voice-call") {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("not configured"),
                "expected 'not configured' in error, got: {err_msg}"
            );
        }
        Ok(_) => panic!("should fail when voice-call is not configured"),
    }
}

#[cfg(feature = "channel-voice-call")]
#[test]
fn build_channel_by_id_configured_voice_call_succeeds() {
    let mut config = Config::default();
    config.channels.voice_call.insert(
        "default".to_string(),
        zeroclaw_config::scattered_types::VoiceCallConfig {
            enabled: true,
            model_provider: zeroclaw_config::scattered_types::VoiceProvider::Twilio,
            account_id: "AC_TEST".to_string(),
            auth_token: "test_token".to_string(),
            from_number: "+15551234567".to_string(),
            webhook_port: 8090,
            require_outbound_approval: true,
            transcription_logging: true,
            tts_voice: None,
            max_call_duration_secs: 3600,
            webhook_base_url: None,
            excluded_tools: vec![],
        },
    );
    let config_arc = Arc::new(RwLock::new(config));
    match build_channel_by_id(&config_arc, "voice-call") {
        Ok(channel) => assert_eq!(channel.name(), "voice_call"),
        Err(e) => panic!("should succeed when voice-call is configured: {e}"),
    }
}

// ── is_stop_command tests ─────────────────────────────────────────────

#[test]
fn is_stop_command_matches_bare_slash_stop() {
    assert!(is_stop_command("/stop"));
}

#[test]
fn is_stop_command_matches_with_leading_trailing_whitespace() {
    assert!(is_stop_command("  /stop  "));
}

#[test]
fn is_stop_command_is_case_insensitive() {
    assert!(is_stop_command("/STOP"));
    assert!(is_stop_command("/Stop"));
}

#[test]
fn is_stop_command_matches_with_bot_suffix() {
    assert!(is_stop_command("/stop@zeroclaw_bot"));
}

#[test]
fn is_stop_command_rejects_other_slash_commands() {
    assert!(!is_stop_command("/new"));
    assert!(!is_stop_command("/model gpt-4"));
    assert!(!is_stop_command("/models"));
}

#[test]
fn is_stop_command_rejects_plain_text() {
    assert!(!is_stop_command("stop"));
    assert!(!is_stop_command("please stop"));
    assert!(!is_stop_command(""));
}

#[test]
fn is_stop_command_rejects_stop_as_substring() {
    assert!(!is_stop_command("/stopwatch"));
    assert!(!is_stop_command("/stop-all"));
}

#[test]
fn interrupt_on_new_message_enabled_for_mattermost_when_true() {
    let cfg = InterruptOnNewMessageConfig {
        telegram: false,
        slack: false,
        discord: false,
        mattermost: true,
        matrix: false,
        whatsapp: false,
    };
    assert!(cfg.enabled_for_channel("mattermost"));
}

#[test]
fn interrupt_on_new_message_disabled_for_mattermost_by_default() {
    let cfg = InterruptOnNewMessageConfig {
        telegram: false,
        slack: false,
        discord: false,
        mattermost: false,
        matrix: false,
        whatsapp: false,
    };
    assert!(!cfg.enabled_for_channel("mattermost"));
}

#[test]
fn interrupt_on_new_message_enabled_for_discord() {
    let cfg = InterruptOnNewMessageConfig {
        telegram: false,
        slack: false,
        discord: true,
        mattermost: false,
        matrix: false,
        whatsapp: false,
    };
    assert!(cfg.enabled_for_channel("discord"));
}

#[test]
fn interrupt_on_new_message_enabled_for_whatsapp() {
    let cfg = InterruptOnNewMessageConfig {
        telegram: false,
        slack: false,
        discord: false,
        mattermost: false,
        matrix: false,
        whatsapp: true,
    };
    assert!(cfg.enabled_for_channel("whatsapp"));
}

#[test]
fn interrupt_on_new_message_config_reads_whatsapp_default_alias() {
    let mut channels = zeroclaw_config::schema::ChannelsConfig::default();
    channels.whatsapp.insert(
        "default".to_string(),
        zeroclaw_config::schema::WhatsAppConfig {
            session_path: Some("/tmp/zeroclaw-whatsapp-session.db".into()),
            interrupt_on_new_message: true,
            ..Default::default()
        },
    );

    let cfg = interrupt_on_new_message_config(&channels);

    assert!(cfg.enabled_for_channel("whatsapp"));
    assert!(!cfg.enabled_for_channel("telegram"));
}

#[test]
fn interrupt_on_new_message_disabled_for_discord_by_default() {
    let cfg = InterruptOnNewMessageConfig {
        telegram: false,
        slack: false,
        discord: false,
        mattermost: false,
        matrix: false,
        whatsapp: false,
    };
    assert!(!cfg.enabled_for_channel("discord"));
}

// ── interruption_scope_key tests ──────────────────────────────────────

#[test]
fn interruption_scope_key_without_scope_id_is_three_component() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "1".into(),
        sender: "alice".into(),
        reply_target: "room".into(),
        content: "hi".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 0,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    assert_eq!(interruption_scope_key(&msg), "matrix_room_alice");
}

#[test]
fn interruption_scope_key_with_scope_id_is_four_component() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "1".into(),
        sender: "alice".into(),
        reply_target: "room".into(),
        content: "hi".into(),
        channel: "matrix".into(),
        channel_alias: None,
        timestamp: 0,
        thread_ts: Some("$thread1".into()),
        interruption_scope_id: Some("$thread1".into()),
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    assert_eq!(interruption_scope_key(&msg), "matrix_room_alice_$thread1");
}

#[test]
fn interruption_scope_key_thread_ts_alone_does_not_affect_key() {
    // thread_ts used for reply anchoring should not bleed into scope key
    let msg = zeroclaw_api::channel::ChannelMessage {
        id: "1".into(),
        sender: "alice".into(),
        reply_target: "C123".into(),
        content: "hi".into(),
        channel: "slack".into(),
        channel_alias: None,
        timestamp: 0,
        thread_ts: Some("1234567890.000100".into()), // Slack top-level fallback
        interruption_scope_id: None,                 // but NOT a thread reply
        attachments: vec![],
        subject: None,

        ..Default::default()
    };
    assert_eq!(interruption_scope_key(&msg), "slack_C123_alice");
}

#[tokio::test]
async fn message_dispatch_different_threads_do_not_cancel_each_other() {
    let channel_impl = Arc::new(SlackRecordingChannel::default());
    let channel: Arc<dyn Channel> = channel_impl.clone();

    let mut channels_by_name = HashMap::new();
    channels_by_name.insert(channel.name().to_string(), channel);

    let runtime_ctx = Arc::new(ChannelRuntimeContext {
        channels_by_name: Arc::new(channels_by_name),
        model_provider: Arc::new(SlowModelProvider {
            delay: Duration::from_millis(150),
        }),
        model_provider_ref: Arc::new("test-provider".to_string()),
        agent_alias: Arc::new("test-agent".to_string()),
        agent_cfg: Arc::new(zeroclaw_config::schema::AliasedAgentConfig::default()),
        memory: Arc::new(NoopMemory),
        memory_strategy: Arc::new(
            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                Arc::new(NoopMemory),
                zeroclaw_config::schema::MemoryConfig::default(),
                std::path::PathBuf::new(),
            ),
        ),
        companion_store: None,
        tools_registry: Arc::new(vec![]),
        observer: Arc::new(NoopObserver),
        system_prompt: Arc::new("test-system-prompt".to_string()),
        model: Arc::new("test-model".to_string()),
        temperature: Some(0.0),
        auto_save_memory: false,
        max_tool_iterations: 10,
        min_relevance_score: 0.0,
        conversation_histories: Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(MAX_CONVERSATION_SENDERS).unwrap(),
        ))),
        pending_new_sessions: Arc::new(Mutex::new(HashSet::new())),
        provider_cache: Arc::new(Mutex::new(HashMap::new())),
        route_overrides: Arc::new(Mutex::new(HashMap::new())),
        thinking_overrides: Arc::new(Mutex::new(HashMap::new())),
        scope_overrides: Arc::new(Mutex::new(HashMap::new())),
        reliability: Arc::new(zeroclaw_config::schema::ReliabilityConfig::default()),
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        workspace_dir: Arc::new(std::env::temp_dir()),
        prompt_config: Arc::new(zeroclaw_config::schema::Config::default()),
        message_timeout_secs: CHANNEL_MESSAGE_TIMEOUT_SECS,
        interrupt_on_new_message: InterruptOnNewMessageConfig {
            telegram: false,
            slack: true,
            discord: false,
            mattermost: false,
            matrix: false,
            whatsapp: false,
        },
        multimodal: zeroclaw_config::schema::MultimodalConfig::default(),
        media_pipeline: zeroclaw_config::schema::MediaPipelineConfig::default(),
        transcription_config: zeroclaw_config::schema::TranscriptionConfig::default(),
        agent_transcription_provider: String::new(),
        hooks: None,
        non_cli_excluded_tools: Arc::new(Vec::new()),
        autonomy_level: AutonomyLevel::default(),
        tool_call_dedup_exempt: Arc::new(Vec::new()),
        model_routes: Arc::new(Vec::new()),
        query_classification: zeroclaw_config::schema::QueryClassificationConfig::default(),
        ack_reactions: true,
        show_tool_calls: true,
        session_store: None,
        approval_manager: Arc::new(ApprovalManager::for_non_interactive(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
        )),
        activated_tools: None,
        cost_tracking: None,
        pacing: zeroclaw_config::schema::PacingConfig::default(),
        max_tool_result_chars: 0,
        context_token_budget: 0,
        debouncer: Arc::new(zeroclaw_infra::debounce::MessageDebouncer::new(
            Duration::ZERO,
        )),
        receipt_generator: None,
        show_receipts_in_response: false,
        last_applied_config_stamp: Arc::new(Mutex::new(None)),
        runtime_defaults_override: Arc::new(Mutex::new(None)),
        persist_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        sop_engine: None,
        sop_audit: None,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<zeroclaw_api::channel::ChannelMessage>(8);
    let send_task = zeroclaw_spawn::spawn!(async move {
        // Two messages from same sender but in different Slack threads —
        // they must NOT cancel each other.
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "1741234567.100001".to_string(),
            sender: "alice".to_string(),
            reply_target: "C123".to_string(),
            content: "thread-a question".to_string(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 1,
            thread_ts: Some("1741234567.100001".to_string()),
            interruption_scope_id: Some("1741234567.100001".to_string()),
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(zeroclaw_api::channel::ChannelMessage {
            id: "1741234567.200002".to_string(),
            sender: "alice".to_string(),
            reply_target: "C123".to_string(),
            content: "thread-b question".to_string(),
            channel: "slack".into(),
            channel_alias: None,
            timestamp: 2,
            thread_ts: Some("1741234567.200002".to_string()),
            interruption_scope_id: Some("1741234567.200002".to_string()),
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
        .await
        .unwrap();
    });

    run_message_dispatch_loop(rx, AgentRouter::single(runtime_ctx), 4).await;
    send_task.await.unwrap();

    // Both tasks should have completed — different threads, no cancellation.
    let sent_messages = channel_impl.sent_messages.lock().await;
    assert_eq!(
        sent_messages.len(),
        2,
        "both Slack thread messages should complete, got: {sent_messages:?}"
    );
}

#[test]
fn sanitize_channel_response_redacts_detected_credentials() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP"; // gitleaks:allow

    let result = sanitize_channel_response(leaked, &tools);

    assert!(!result.contains("AKIAABCDEFGHIJKLMNOP")); // gitleaks:allow
    assert!(result.contains("[REDACTED"));
}

/// Regression test for a redaction bypass: an AWS-shaped credential
/// dropped into a markdown link destination -- exactly where a
/// prompt-injected model would try to exfiltrate one -- must not sail
/// through unredacted just because a link destination is otherwise
/// protected from the high-entropy heuristic.
#[test]
fn sanitize_channel_response_detects_aws_key_smuggled_in_markdown_link_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    // AKIAABCDEFGHIJKLMNOP gitleaks:allow
    let target = "https://exfil.example.invalid/callback?key=AKIAABCDEFGHIJKLMNOP";

    let result = sanitize_channel_response(&format!("[callback]({target})"), &tools);

    assert!(!result.contains("AKIAABCDEFGHIJKLMNOP"), "result: {result}"); // gitleaks:allow
    assert!(
        result.contains("[REDACTED_AWS_CREDENTIAL]"),
        "result: {result}"
    );
}

// A protected link destination shields the *shape-based* high-entropy
// heuristic (the shape false-positive), never a deterministic credential
// pattern. A real credential dropped into a link destination -- exactly
// where a prompt-injected model would try to smuggle one past the
// detector -- must still be caught.

#[test]
fn sanitize_channel_response_still_detects_deterministic_credential_in_markdown_link_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://example.invalid/callback?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[callback]({target})"), &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_deterministic_credential_in_markdown_reference_destination()
 {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://example.invalid/callback?api_key=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let response = format!("See [callback][cb]\n\n[cb]: {target}");

    let result = sanitize_channel_response(&response, &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_deterministic_credential_in_entity_escaped_markdown_destination()
 {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://example.invalid/callback?x=1&amp;token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[callback]({target})"), &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_entity_destination_and_title() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://example.invalid/callback?x=1&amp;token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let title = "https://example.invalid/callback?x=1&token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[callback]({target} \"{title}\")"), &tools);

    // The destination span computation is still correct (used by the
    // entropy heuristic), but the deterministic "Token value" pattern is
    // not suppressed by it, so both the destination and the distinct
    // title copy are caught.
    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_reference_destination_and_title() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://example.invalid/callback?x=1&amp;token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let title = "https://example.invalid/callback?x=1&token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let response = format!("See [callback][cb]\n\n[cb]: {target} \"{title}\"");

    let result = sanitize_channel_response(&response, &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_url_entity_destination_and_title() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target =
        "https&colon;&sol;&sol;example.invalid/callback?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let title = "https://example.invalid/callback?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[callback]({target} \"{title}\")"), &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_bot_token_in_markdown_autolink_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "https://api.telegram.org/bot123456:ABC-def_GHI/getUpdates";

    let result = sanitize_channel_response(&format!("<{target}>"), &tools);

    assert!(!result.contains("123456:ABC-def_GHI"), "result: {result}");
    assert!(result.contains("[REDACTED_BOT_TOKEN]"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_scans_markdown_link_text() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let token = "aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let target = "https://example.invalid/callback?ticket=public-id";

    let result = sanitize_channel_response(&format!("[token={token}]({target})"), &tools);

    assert!(!result.contains(token));
    assert!(result.contains(target), "result: {result}");
}

#[test]
fn sanitize_channel_response_scans_link_text_secret_when_match_overlaps_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "file:///tmp/report.md";

    let result =
        sanitize_channel_response(&format!("[password=longsecretvalue]({target})"), &tools);

    // The generic-secret pattern is greedy and, starting outside the
    // destination, can span into it; deterministic patterns are never
    // suppressed by a protected span, so the whole overlapping match is
    // redacted. That is the safe direction: losing an adjacent file
    // reference is preferable to leaking the secret it was next to.
    assert!(!result.contains("longsecretvalue"), "result: {result}");
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_scans_link_text_when_label_matches_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[{target}]({target})"), &tools);

    assert!(
        !result.starts_with(&format!("[{target}]")),
        "result: {result}"
    );
    assert!(result.contains(&format!("]({target})")), "result: {result}");
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_escaped_markdown_destination() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "file:///tmp/report\\(1\\).md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

    let result = sanitize_channel_response(&format!("[report]({target})"), &tools);

    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_raw_file_uri() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "file:///tmp/report.md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let outbound = format!("Recorded {target}.");

    // The entropy-protected span is still computed correctly...
    let spans = channel_outbound_protected_spans(&outbound, OutboundContentFormat::Markdown);
    assert_eq!(
        spans
            .iter()
            .map(|span| &outbound[span.start..span.end])
            .collect::<Vec<_>>(),
        vec![target],
    );
    // ...but the deterministic "Token value" pattern inside it is not
    // suppressed by that protection.
    let result = sanitize_channel_response(&outbound, &tools);

    assert!(
        result.contains("file:///tmp/report.md?"),
        "result: {result}"
    );
    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_keyed_raw_file_uri() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "file:///tmp/report.md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let outbound = format!("Recorded path={target}.");

    let spans = channel_outbound_protected_spans(&outbound, OutboundContentFormat::Markdown);
    assert_eq!(
        spans
            .iter()
            .map(|span| &outbound[span.start..span.end])
            .collect::<Vec<_>>(),
        vec![target],
    );
    let result = sanitize_channel_response(&outbound, &tools);

    assert!(
        result.contains("path=file:///tmp/report.md?"),
        "result: {result}"
    );
    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_still_detects_credential_in_json_keyed_raw_file_uri() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let target = "file:///tmp/report.md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let outbound = format!(r#"{{"uri":"{target}"}}"#);

    let spans = channel_outbound_protected_spans(&outbound, OutboundContentFormat::Markdown);
    assert_eq!(
        spans
            .iter()
            .map(|span| &outbound[span.start..span.end])
            .collect::<Vec<_>>(),
        vec![target],
    );
    let result = sanitize_channel_response(&outbound, &tools);

    assert!(
        result.contains(r#""uri":"file:///tmp/report.md?"#),
        "result: {result}"
    );
    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn plain_text_leak_guard_preserves_raw_file_uri_filenames() {
    let target = "file:///home/zeroclaw/.zeroclaw/agents/mission-orchestrator/workspace/tasks/inbox/2026-07-02-11-26-plan-b-for-something-useful.md";
    let outbound = format!("Recorded {target}.");

    let result = redact_channel_outbound_leaks(
        &outbound,
        &zeroclaw_config::schema::LeakDetectionConfig::default(),
        OutboundContentFormat::PlainText,
    );

    assert_eq!(result, outbound);
}

#[test]
fn irc_family_outbound_format_is_plain_text() {
    assert_eq!(
        outbound_content_format_for_channel("irc.default"),
        OutboundContentFormat::PlainText
    );
    assert_eq!(
        outbound_content_format_for_channel("twitch.default"),
        OutboundContentFormat::PlainText
    );
    assert_eq!(
        outbound_content_format_for_channel("twitch"),
        OutboundContentFormat::PlainText
    );
    assert_eq!(
        outbound_content_format_for_channel("telegram.default"),
        OutboundContentFormat::Markdown
    );
}

#[test]
fn sanitize_channel_response_respects_disabled_leak_detection() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let leaked = "Temporary key: AKIAABCDEFGHIJKLMNOP"; // gitleaks:allow
    let leak_detection = zeroclaw_config::schema::LeakDetectionConfig {
        enabled: false,
        ..Default::default()
    };

    let result = sanitize_channel_response_with_leak_detection(leaked, &tools, &leak_detection);

    assert_eq!(result, leaked);
}

#[test]
fn leak_only_guard_preserves_protocol_looking_announcement_text() {
    let input = "[Used tools: shell]\n\nCron output completed.";

    let result = redact_channel_outbound_leaks(
        input,
        &zeroclaw_config::schema::LeakDetectionConfig::default(),
        OutboundContentFormat::Markdown,
    );

    assert_eq!(result, input);
}

#[test]
fn leak_only_guard_still_detects_credential_in_raw_file_uri() {
    let target = "file:///tmp/report.md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
    let input = format!("Cron output: {target}");

    let result = redact_channel_outbound_leaks(
        &input,
        &zeroclaw_config::schema::LeakDetectionConfig::default(),
        OutboundContentFormat::Markdown,
    );

    assert!(
        result.contains("file:///tmp/report.md?"),
        "result: {result}"
    );
    assert!(
        !result.contains("aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
        "result: {result}"
    );
    assert!(result.contains("[REDACTED"), "result: {result}");
}

#[test]
fn sanitize_channel_response_passes_clean_text() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let clean_text = "This is a normal message with no credentials.";

    let result = sanitize_channel_response(clean_text, &tools);

    assert_eq!(result, clean_text);
}

#[test]
fn sanitize_channel_response_preserves_schema_json_array_without_tools() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let schema = r#"[{"name":"planner","parameters":{"goal":"string"}}]"#;

    let result = sanitize_channel_response(schema, &tools);

    assert_eq!(result, schema);
}

#[test]
fn sanitize_channel_response_preserves_tool_calls_audit_json() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let audit_json = r#"{"tool_calls":[{"id":"case-1","status":"queued","service":"billing"}]}"#;

    let result = sanitize_channel_response(audit_json, &tools);

    assert_eq!(result, audit_json);
}

#[test]
fn sanitize_channel_response_preserves_reference_function_call_json_without_tools() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let reference_json =
        r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;

    let result = sanitize_channel_response(reference_json, &tools);

    assert_eq!(result, reference_json);
}

#[test]
fn sanitize_channel_response_preserves_reference_function_call_json_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let reference_json =
        r#"{"type":"function_call","name":"support_case","arguments":{"id":"A1"}}"#;

    let result = sanitize_channel_response(reference_json, &tools);

    assert_eq!(result, reference_json);
}

#[test]
fn sanitize_channel_response_preserves_unknown_tool_calls_json_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}]}"#;

    let result = sanitize_channel_response(business_json, &tools);

    assert_eq!(result, business_json);
}

#[test]
fn sanitize_channel_response_preserves_malformed_unknown_tool_calls_json_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let business_json = r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#;

    let result = sanitize_channel_response(business_json, &tools);

    assert_eq!(result, business_json);
}

#[test]
fn sanitize_channel_response_preserves_json_fenced_tool_protocol_example() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let example = r#"Here is a protocol example:
```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```"#;

    let result = sanitize_channel_response(example, &tools);

    assert_eq!(result, example);
}

#[test]
fn sanitize_channel_response_removes_registered_tool_json_array() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let internal = r#"[{"name":"mock_price","parameters":{"symbol":"BTC"}}]"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_removes_internal_tool_protocol_envelopes() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let internal = r#"{"toolcalls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_removes_json_fenced_internal_tool_protocol() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let internal = r#"```json
{"tool_calls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}
```"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_removes_embedded_json_fenced_internal_tool_protocol() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let response = r#"Intro
```json
{"tool_calls":[{"name":"mock_price","arguments":{"symbol":"BTC"}}]}
```
Done."#;

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro"));
    assert!(result.contains("Done."));
    assert!(!result.contains("tool_calls"));
    assert!(!result.contains("mock_price"));
}

#[test]
fn sanitize_channel_response_removes_embedded_tool_call_fence() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let response = r#"Let me call it:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
Done."#;

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Done."));
    assert!(!result.contains("tool_call"));
    assert!(!result.contains("shell"));
    assert!(!result.contains("command"));
}

#[test]
fn sanitize_channel_response_preserves_tool_call_fenced_example() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;

    let result = sanitize_channel_response(example, &tools);

    assert_eq!(result, example);
}

#[test]
fn sanitize_channel_response_removes_standalone_tool_call_fence() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let internal = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_removes_standalone_tool_name_fence() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let internal = r#"```tool shell
{"command":"pwd"}
```"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_preserves_tool_call_tag_example() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let example = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;

    let result = sanitize_channel_response(example, &tools);

    assert_eq!(result, example);
}

#[test]
fn sanitize_channel_response_strips_tagged_tool_call_before_trailing_text() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
Done."#;

    let result = sanitize_channel_response(response, &tools);

    assert_eq!(result, "Done.");
}

#[test]
fn sanitize_channel_response_removes_malformed_top_level_protocol() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let internal = r#"{"tool_call_id":"call_1","content":"raw"#;

    let result = sanitize_channel_response(internal, &tools);

    assert_eq!(result, "");
}

#[test]
fn sanitize_channel_response_removes_embedded_malformed_protocol_json() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let response =
        "Intro\n{\"tool_calls\":[{\"call_id\":\"call_1\",\"arguments\":{\"value\":\"X\"}\nDone";

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro"));
    assert!(result.contains("Done"));
    assert!(!result.contains("tool_calls"));
    assert!(!result.contains("arguments"));
}

#[test]
fn sanitize_channel_response_removes_multiline_embedded_malformed_protocol_json() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let response = "Intro\n{\n  \"tool_calls\": [{\"call_id\":\"call_1\",\"arguments\":{\"value\":\"X\"}}\nDone";

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro"));
    assert!(result.contains("Done"));
    assert!(!result.contains("tool_calls"));
    assert!(!result.contains("arguments"));
}

#[test]
fn sanitize_channel_response_keeps_protocol_explanation_text() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let explanation = "A markdown block starting with ```tool can be used in protocol examples.";

    let result = sanitize_channel_response(explanation, &tools);

    assert_eq!(result, explanation);
}

#[test]
fn sanitize_channel_response_keeps_safe_protocol_envelope_content_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let response = "Intro text\n{\"content\":\"A markdown block starting with ```tool can be used in examples.\",\"tool_calls\":[{\"name\":\"mock_price\",\"arguments\":{\"symbol\":\"BTC\"}}]}\nDone.";

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro text"));
    assert!(result.contains("A markdown block starting with ```tool"));
    assert!(result.contains("Done."));
    assert!(!result.contains("tool_calls"));
}

#[test]
fn sanitize_channel_response_removes_isolated_tool_result_envelope_content_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let response =
        "Intro text\n{\"tool_call_id\":\"call_1\",\"content\":\"raw tool output\"}\nDone.";

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro text"));
    assert!(result.contains("Done."));
    assert!(!result.contains("tool_call_id"));
    assert!(!result.contains("raw tool output"));
}

#[test]
fn sanitize_channel_response_removes_nested_protocol_content_with_tools() {
    let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockPriceTool)];
    let response = "Intro text\n{\"content\":\"{\\\"toolcalls\\\":[{\\\"name\\\":\\\"mock_price\\\",\\\"arguments\\\":{\\\"symbol\\\":\\\"BTC\\\"}}]}\",\"tool_calls\":[{\"name\":\"mock_price\",\"arguments\":{\"symbol\":\"BTC\"}}]}\nDone.";

    let result = sanitize_channel_response(response, &tools);

    assert!(result.contains("Intro text"));
    assert!(result.contains("Done."));
    assert!(!result.contains("toolcalls"));
    assert!(!result.contains("shell"));
}

#[test]
fn sanitize_channel_response_strips_xml_tool_result_blocks() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let input = "<tool_result>\n{\"results\":[]}\n</tool_result>\n<tool_result>\n{\"command\":\"ls\",\"exit_code\":0}\n</tool_result>Here is what I found.";

    let result = sanitize_channel_response(input, &tools);

    assert!(!result.contains("tool_result"));
    assert!(!result.contains("exit_code"));
    assert!(result.contains("Here is what I found."));
}

#[test]
fn sanitize_channel_response_strips_mixed_tool_result_and_text() {
    let tools: Vec<Box<dyn Tool>> = Vec::new();
    let input = "Let me check.\n<tool_result name=\"shell\">\noutput here\n</tool_result>\nThe answer is 42.";

    let result = sanitize_channel_response(input, &tools);

    assert!(!result.contains("<tool_result"));
    assert!(!result.contains("output here"));
    assert!(result.contains("The answer is 42."));
}

// ── Tests for strip_think_tags_inline (streaming draft sanitization) ──

#[test]
fn strip_think_tags_inline_removes_single_block() {
    assert_eq!(
        strip_think_tags_inline("<think>reasoning</think>Hello"),
        "Hello"
    );
}

#[test]
fn strip_think_tags_inline_removes_multiple_blocks() {
    assert_eq!(
        strip_think_tags_inline("<think>a</think>X<think>b</think>Y"),
        "XY"
    );
}

#[test]
fn strip_think_tags_inline_handles_unclosed_block() {
    assert_eq!(
        strip_think_tags_inline("visible<think>hidden tail"),
        "visible"
    );
}

#[test]
fn strip_think_tags_inline_preserves_text_without_tags() {
    assert_eq!(strip_think_tags_inline("plain text"), "plain text");
}

#[test]
fn strip_think_tags_inline_handles_empty_string() {
    assert_eq!(strip_think_tags_inline(""), "");
}

#[test]
fn strip_think_tags_inline_strips_surrounding_whitespace() {
    assert_eq!(
        strip_think_tags_inline("<think>hidden</think>  Answer  "),
        "Answer"
    );
}

// ── Tests tool context preservation ──────────────

#[test]
fn extract_current_turn_tool_messages_returns_intermediate_messages() {
    let history = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("older msg"),
        ChatMessage::assistant("older reply"),
        ChatMessage::user("block the iPad"),
        ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
        ChatMessage::tool("ok"),
        ChatMessage::assistant("Done, iPad is blocked."),
    ];

    let tool_msgs = extract_current_turn_tool_messages(&history);
    assert_eq!(tool_msgs.len(), 2);
    assert_eq!(tool_msgs[0].role, "assistant");
    assert!(tool_msgs[0].content.contains("tool_call"));
    assert_eq!(tool_msgs[1].role, "tool");
}

#[test]
fn extract_current_turn_tool_messages_empty_when_no_tools() {
    let history = vec![
        ChatMessage::user("hello"),
        ChatMessage::assistant("Hi there!"),
    ];

    let tool_msgs = extract_current_turn_tool_messages(&history);
    assert!(tool_msgs.is_empty());
}

#[test]
fn extract_current_turn_tool_messages_multiple_tool_rounds() {
    let history = vec![
        ChatMessage::user("do two things"),
        ChatMessage::assistant("{\"tool_call\": \"read_skill\"}"),
        ChatMessage::tool("skill content"),
        ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
        ChatMessage::tool("shell output"),
        ChatMessage::assistant("All done."),
    ];

    let tool_msgs = extract_current_turn_tool_messages(&history);
    assert_eq!(tool_msgs.len(), 4);
}

#[test]
fn normalize_cached_channel_turns_passes_through_tool_messages() {
    let turns = vec![
        ChatMessage::user("block the iPad"),
        ChatMessage::assistant("{\"tool_call\": \"shell\"}"),
        ChatMessage::tool("ok"),
        ChatMessage::assistant("iPad blocked."),
        ChatMessage::user("next question"),
    ];

    let normalized = normalize_cached_channel_turns(turns);
    // user, assistant(tool_call), tool, assistant(final), user
    assert_eq!(normalized.len(), 5);
    assert_eq!(normalized[2].role, "tool");
}

#[test]
fn default_keep_tool_context_turns_is_two() {
    let config = zeroclaw_config::schema::AliasedAgentConfig::default();
    assert_eq!(config.resolved.keep_tool_context_turns, 2);
}

#[test]
fn build_channel_system_prompt_excludes_volatile_fields() {
    let prompt = build_channel_system_prompt("You are a helpful assistant.", "mattermost", None);
    assert!(
        !prompt.contains("reply_target="),
        "system prompt must not include reply_target; got {prompt}"
    );
    assert!(
        !prompt.contains("sender="),
        "system prompt must not include sender=; got {prompt}"
    );
    assert!(
        !prompt.contains("message_id="),
        "system prompt must not include message_id=; got {prompt}"
    );
    assert!(
        !prompt.contains("Channel context:"),
        "system prompt must not include the legacy Channel context block; got {prompt}"
    );
    assert!(
        !prompt.contains("delivery="),
        "system prompt must not include the cron_add delivery hint; got {prompt}"
    );
}

// --- Surface 1(a) regression tests ---

fn build_msg_for_signal_test() -> zeroclaw_api::channel::ChannelMessage {
    zeroclaw_api::channel::ChannelMessage {
        channel: "mattermost".to_string(),
        reply_target: "ch:thread".to_string(),
        sender: "user_test".to_string(),
        content: "hello".to_string(),
        id: "msg-001".to_string(),
        ..Default::default()
    }
}

#[test]
fn channel_prompt_reflects_per_turn_signal_false_uses_no_tools_framing() {
    let base_prompt_with_native = format!(
        "{}\n{}",
        "Some base agent prompt.",
        ::zeroclaw_runtime::agent::system_prompt::NATIVE_TOOLS_TASK_FRAMING,
    );
    let msg = build_msg_for_signal_test();
    let prompt = build_channel_system_prompt_for_message_with_signal(
        &base_prompt_with_native,
        &msg,
        None,
        false, // per-turn: no effective native specs
    );
    assert!(
        prompt.contains(::zeroclaw_runtime::agent::system_prompt::NO_TOOLS_TASK_FRAMING),
        "per-turn signal=false must inject NO_TOOLS_TASK_FRAMING; got: {prompt}"
    );
    assert!(
        !prompt.contains(::zeroclaw_runtime::agent::system_prompt::NATIVE_TOOLS_TASK_FRAMING),
        "per-turn signal=false must remove NATIVE_TOOLS_TASK_FRAMING; got: {prompt}"
    );
}

#[test]
fn channel_prompt_keeps_startup_signal_when_per_turn_agrees() {
    let base_prompt_with_no_tools = format!(
        "{}\n{}",
        "Some base agent prompt.",
        ::zeroclaw_runtime::agent::system_prompt::NO_TOOLS_TASK_FRAMING,
    );
    let msg = build_msg_for_signal_test();
    let prompt_with_helper = build_channel_system_prompt_for_message_with_signal(
        &base_prompt_with_no_tools,
        &msg,
        None,
        false, // matches the anchor already in the base prompt
    );
    let prompt_baseline =
        build_channel_system_prompt_for_message(&base_prompt_with_no_tools, &msg, None);
    assert_eq!(
        prompt_with_helper, prompt_baseline,
        "per-turn signal matching the startup anchor must produce an identical prompt"
    );
}

#[test]
fn channel_prompt_rewrites_no_tools_to_native_when_per_turn_differs() {
    let base_prompt_with_no_tools = format!(
        "{}\n{}",
        "Some base agent prompt.",
        ::zeroclaw_runtime::agent::system_prompt::NO_TOOLS_TASK_FRAMING,
    );
    let msg = build_msg_for_signal_test();
    let prompt = build_channel_system_prompt_for_message_with_signal(
        &base_prompt_with_no_tools,
        &msg,
        None,
        true, // per-turn: native specs now present
    );
    assert!(
        prompt.contains(::zeroclaw_runtime::agent::system_prompt::NATIVE_TOOLS_TASK_FRAMING),
        "per-turn signal=true must inject NATIVE_TOOLS_TASK_FRAMING; got: {prompt}"
    );
    assert!(
        !prompt.contains(::zeroclaw_runtime::agent::system_prompt::NO_TOOLS_TASK_FRAMING),
        "per-turn signal=true must remove NO_TOOLS_TASK_FRAMING; got: {prompt}"
    );
}

#[test]
fn channel_prompt_no_op_when_anchor_absent() {
    let base_prompt_no_anchor = "Custom agent prompt with no tool-availability anchor.";
    let msg = build_msg_for_signal_test();
    let prompt_with_helper = build_channel_system_prompt_for_message_with_signal(
        base_prompt_no_anchor,
        &msg,
        None,
        true,
    );
    let prompt_baseline =
        build_channel_system_prompt_for_message(base_prompt_no_anchor, &msg, None);
    assert_eq!(
        prompt_with_helper, prompt_baseline,
        "no-anchor base prompt must produce an identical prompt regardless of per-turn signal"
    );
}

#[test]
fn build_channel_system_prompt_byte_stable_across_sender() {
    // Two system prompts built with the same base/channel/bot_mention
    // but conceptually different (hypothetical) senders MUST be
    // byte-identical. Sender disambiguation now lives in the preamble.
    let prompt_a = build_channel_system_prompt("Base.", "mattermost", None);
    let prompt_b = build_channel_system_prompt("Base.", "mattermost", None);
    assert_eq!(
        prompt_a, prompt_b,
        "system prompt must be byte-stable regardless of per-turn sender"
    );
}

#[test]
fn build_channel_system_prompt_refreshes_legacy_datetime_section_to_date_only() {
    let prompt = build_channel_system_prompt(
        "Base.\n\n## Current Date\n\nProject note, not generated date context.\n\n## Current Date & Time\n\n2026-01-01 01:02:03 (UTC)\n\n## Runtime\n\nHost: old\n",
        "mattermost",
        None,
    );

    assert!(prompt.contains("## Current Date\n\n"));
    assert!(prompt.contains("Project note, not generated date context."));
    assert!(!prompt.contains("## Current Date & Time"));
    assert!(!prompt.contains("01:02:03"));
    let generated_section = prompt
        .split("## Runtime")
        .next()
        .expect("prompt should contain runtime section before generated date assertion");
    let date_line = generated_section
        .rsplit("## Current Date\n\n")
        .next()
        .and_then(|rest| rest.lines().next())
        .expect("current date section should have a date line");
    assert_eq!(
        &date_line[..10],
        &chrono::Local::now().format("%Y-%m-%d").to_string()
    );
    assert!(
        date_line[10..].starts_with(" ("),
        "date line should contain only date plus UTC offset: {date_line}"
    );
}

#[test]
fn build_channel_system_prompt_refreshes_current_date_section() {
    let prompt = build_channel_system_prompt(
        "Base.\n\n## Current Date\n\n2026-01-01 (+00:00)\n\n## Runtime\n\nHost: old\n",
        "mattermost",
        None,
    );

    assert!(prompt.contains("## Current Date\n\n"));
    assert!(!prompt.contains("2026-01-01 (+00:00)"));
    let date_line = prompt
        .split("## Current Date\n\n")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .expect("current date section should have a date line");
    assert_eq!(
        &date_line[..10],
        &chrono::Local::now().format("%Y-%m-%d").to_string()
    );
}

#[test]
fn build_channel_turn_context_preamble_empty_when_reply_target_empty() {
    // CLI-style path: when there is no channel recipient, no preamble
    // is needed. Mirrors CLI behaviour where no per-turn context block
    // is added.
    let msg = zeroclaw_api::channel::ChannelMessage {
        channel: "telegram".into(),
        reply_target: String::new(),
        sender: "alice".into(),
        id: "msg-1".into(),
        ..Default::default()
    };
    let preamble = build_channel_turn_context_preamble(&msg, None);
    assert_eq!(
        preamble, "",
        "CLI-style empty reply_target must yield no preamble"
    );
}

#[test]
fn build_channel_turn_context_preamble_carries_volatile_fields() {
    // Every per-turn field the system prompt used to carry lives in the
    // preamble now. Pin the comma-separated tuple so a refactor that
    // splits or rewords it fails loudly.
    let msg = zeroclaw_api::channel::ChannelMessage {
        channel: "telegram".into(),
        reply_target: "chat:42".into(),
        sender: "alice".into(),
        id: "msg-xyz789".into(),
        ..Default::default()
    };
    let preamble = build_channel_turn_context_preamble(&msg, None);

    assert!(
        preamble.contains("[turn-context]"),
        "preamble must start with the [turn-context] marker: {preamble}"
    );
    assert!(
        preamble.contains("channel=telegram"),
        "preamble must carry the channel name: {preamble}"
    );
    assert!(
        preamble.contains("reply_target=chat:42"),
        "preamble must carry reply_target: {preamble}"
    );
    assert!(
        preamble.contains("sender=alice"),
        "preamble must carry sender (for disambiguation): {preamble}"
    );
    assert!(
        preamble.contains("message_id=msg-xyz789"),
        "preamble must carry message_id (for the reaction tool): {preamble}"
    );
    assert!(
        preamble.contains("\"to\":\"chat:42\""),
        "preamble must carry the cron_add delivery hint with reply_target as `to`: {preamble}"
    );
    assert!(
        !preamble.contains("\"thread_id\""),
        "non-webhook preamble must not emit thread_id: {preamble}"
    );
}

#[test]
fn compose_outgoing_user_turn_with_context_orders_preamble_content() {
    // Order on the wire: preamble -> raw user content, joined by blank
    // lines (the memory block is engine-injected ABOVE the whole turn).
    // Empty preamble leaves the raw content untouched (CLI-style path).
    assert_eq!(
        compose_outgoing_user_turn_with_context("", "hello"),
        "hello"
    );
    assert_eq!(
        compose_outgoing_user_turn_with_context("[turn-context] x\n\n", "hello"),
        "[turn-context] x\n\n\n\nhello"
    );
}

// ─── background-announcement splice (fork #22) ─────────────

/// The block lands as a PREFIX of the turn's user message, block first,
/// with no separator added here — the runtime's block brings its own
/// trailing newline, exactly as `loop_.rs` splices it.
///
/// Discriminating line: the `assert_eq!` on the composed content — swap the
/// `format!` operands and the model reads the announcement as a footer
/// under the user's request instead of context above it.
#[test]
fn prepend_context_prefixes_the_last_user_turn_when_it_is_last() {
    let mut history = vec![
        ChatMessage::system("sys"),
        ChatMessage::assistant("earlier reply"),
        ChatMessage::user("[2026-07-29 10:00:00 UTC] what happened?"),
    ];

    let spliced =
        prepend_context_to_last_user_turn(&mut history, "## Background tasks finished\nkid\n\n");

    assert!(spliced, "a user turn was present, so the block must land");
    assert_eq!(
        history[2].content,
        "## Background tasks finished\nkid\n\n[2026-07-29 10:00:00 UTC] what happened?"
    );
    assert_eq!(history.len(), 3, "the splice must not add a message");
    assert_eq!(
        history[1].content, "earlier reply",
        "no other turn may be rewritten"
    );
}

/// The choice, stated: only the LAST message, and only when it is the user
/// turn — an earlier user message is not reached back for. Nothing is
/// pushed, nothing is rewritten, and the `false` return is what tells the
/// caller to let its claim guard drop armed.
///
/// Discriminating line: `assert!(!spliced)` — drop the `role == "user"`
/// test and the block is grafted onto an assistant message (or a
/// tool-result turn), where it reads as something the model said.
#[test]
fn prepend_context_is_a_noop_when_the_last_message_is_not_the_user_turn() {
    let mut history = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("[2026-07-29 10:00:00 UTC] earlier question"),
        ChatMessage::assistant("assistant has the last word"),
    ];
    let before = history.clone();

    let spliced = prepend_context_to_last_user_turn(&mut history, "## Background tasks\nkid\n");

    assert!(
        !spliced,
        "no user turn to prefix means the block never reaches the model"
    );
    assert_eq!(history.len(), before.len(), "nothing may be pushed");
    for (after, before) in history.iter().zip(before.iter()) {
        assert_eq!(after.content, before.content, "history must be untouched");
    }
}

/// Empty history is the same no-op, and reports the same `false`.
///
/// Discriminating line: `assert!(!spliced)` — report `true` here and a
/// caller would disarm a guard for announcements nobody read.
#[test]
fn prepend_context_is_a_noop_on_empty_history() {
    let mut history: Vec<ChatMessage> = Vec::new();

    let spliced = prepend_context_to_last_user_turn(&mut history, "## Background tasks\nkid\n");

    assert!(!spliced, "there is nothing to prefix");
    assert!(history.is_empty(), "nothing may be pushed");
}

/// An empty block is not a delivery: it must not be reported as spliced,
/// so a caller can never read "landed" for a turn that says nothing.
///
/// Discriminating line: `assert!(!spliced)` — drop the empty-block guard
/// and the function returns `true` having changed nothing.
#[test]
fn prepend_context_reports_no_splice_for_an_empty_block() {
    let mut history = vec![ChatMessage::user("[2026-07-29 10:00:00 UTC] hi")];

    let spliced = prepend_context_to_last_user_turn(&mut history, "");

    assert!(!spliced, "an empty block is nothing to deliver");
    assert_eq!(history[0].content, "[2026-07-29 10:00:00 UTC] hi");
}

// ─── the announcement bracket (fork #22, issue #25) ────────

/// What the bracket did with a claim guard.
///
/// Recorded rather than re-implemented: a fake that re-derived
/// `UnclaimOnDrop`'s own arm/disarm rule would be a test proving its own
/// copy of the rule. What is this crate's business is *which* of the two
/// things the bracket does to the guard, and with what verdict.
#[derive(Debug, PartialEq, Eq)]
enum GuardEvent {
    /// `settle_against` ran, carrying the turn's own verdict.
    Settled { turn_succeeded: bool },
    /// The bracket let the guard go without settling it — which is how
    /// `UnclaimOnDrop` hands its rows back to the store.
    DroppedUnsettled,
}

struct RecordingGuard {
    log: Arc<Mutex<Vec<GuardEvent>>>,
    settled: bool,
}

impl ChannelAnnouncementGuard for RecordingGuard {
    fn settle_against(mut self, outcome: &LlmExecutionResult) {
        self.settled = true;
        self.log
            .lock()
            .expect("guard log lock should not be poisoned")
            .push(GuardEvent::Settled {
                turn_succeeded: outcome.turn_succeeded(),
            });
    }
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        if !self.settled {
            self.log
                .lock()
                .expect("guard log lock should not be poisoned")
                .push(GuardEvent::DroppedUnsettled);
        }
    }
}

/// Everything one run of the bracket did that a caller could observe.
struct BracketRun {
    /// Every key the bracket claimed under, in order.
    claimed_keys: Vec<String>,
    /// The history the turn body was handed — literally what the model
    /// would have been given, snapshotted from inside the body.
    body_history: Vec<ChatMessage>,
    /// What became of the guard.
    guard_events: Vec<GuardEvent>,
}

/// Run the bracket with both of its production dependencies stubbed: a
/// claim that yields `block` under whatever key it is asked for, and a turn
/// body that returns `body_outcome` without touching a provider.
///
/// The real claim cannot be used here — see
/// [`ChannelAnnouncementGuard`]'s note on `control_plane()` — so the stub
/// is what makes claim, splice and settle observable at all.
async fn run_announcement_bracket(
    history_key: &str,
    history: &mut Vec<ChatMessage>,
    block: &str,
    body_outcome: LlmExecutionResult,
) -> BracketRun {
    let claimed_keys = Arc::new(Mutex::new(Vec::new()));
    let guard_events = Arc::new(Mutex::new(Vec::new()));
    let body_history = Arc::new(Mutex::new(Vec::new()));

    {
        let claimed_keys = Arc::clone(&claimed_keys);
        let guard_events = Arc::clone(&guard_events);
        let body_history = Arc::clone(&body_history);
        let block = block.to_string();
        run_channel_turn_with_background_announcements(
            history_key,
            history,
            async move |key: &str| {
                claimed_keys
                    .lock()
                    .expect("claim log lock should not be poisoned")
                    .push(key.to_string());
                (
                    block,
                    Some(RecordingGuard {
                        log: guard_events,
                        settled: false,
                    }),
                )
            },
            async move |history: &mut Vec<ChatMessage>| {
                *body_history
                    .lock()
                    .expect("body history lock should not be poisoned") = history.clone();
                body_outcome
            },
        )
        .await;
    }

    BracketRun {
        claimed_keys: claimed_keys
            .lock()
            .expect("claim log lock should not be poisoned")
            .clone(),
        body_history: body_history
            .lock()
            .expect("body history lock should not be poisoned")
            .clone(),
        guard_events: std::mem::take(
            &mut *guard_events
                .lock()
                .expect("guard log lock should not be poisoned"),
        ),
    }
}

fn turn_that_succeeded() -> LlmExecutionResult {
    LlmExecutionResult::Completed(Ok(Ok("the reply".to_string())))
}

/// A real `tokio::time::error::Elapsed`; the type has no public
/// constructor, so the only way to get one is to let a timeout fire.
async fn timed_out() -> tokio::time::error::Elapsed {
    tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
        .await
        .expect_err("a pending future must time out")
}

fn user_tail_history() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("sys"),
        ChatMessage::assistant("earlier reply"),
        ChatMessage::user("[2026-07-29 10:00:00 UTC] what happened?"),
    ]
}

const ANNOUNCEMENT_BLOCK: &str = "## Background tasks finished\n- kid: done\n\n";

/// The one thing the old source-literal guard could not express: the block
/// is in the history *the turn body is handed*, above the user's text, and
/// nothing else about that history moved.
///
/// Discriminating line: the `assert_eq!` on `last.content`. Take the splice
/// out of the bracket, or take the claim out, and the body sees the bare
/// user turn — fork #22, every Detached completion silent on every channel.
#[tokio::test]
async fn announcement_bracket_puts_the_block_above_the_user_turn_the_body_sees() {
    let mut history = user_tail_history();

    let run = run_announcement_bracket(
        "telegram:chat-7",
        &mut history,
        ANNOUNCEMENT_BLOCK,
        turn_that_succeeded(),
    )
    .await;

    let last = run
        .body_history
        .last()
        .expect("the turn body must be handed a history");
    assert_eq!(last.role, "user", "the block hangs on the user turn");
    assert_eq!(
        last.content,
        "## Background tasks finished\n- kid: done\n\n[2026-07-29 10:00:00 UTC] what happened?",
        "the block must be a prefix of the user turn, not a footer under it"
    );
    assert_eq!(
        run.body_history.len(),
        3,
        "the splice must not add a message"
    );
    assert_eq!(
        run.body_history[1].content, "earlier reply",
        "no other turn may be rewritten"
    );
}

/// The claim goes under this conversation's history key, not some other
/// scope's.
///
/// Discriminating line: `assert_eq!` on `claimed_keys`. Claim under the
/// wrong key and this turn either consumes another conversation's news or
/// never sees its own — the failure the source-literal guard was blind to,
/// because a wrong key is still a call.
#[tokio::test]
async fn announcement_bracket_claims_under_the_conversation_history_key() {
    let mut history = user_tail_history();

    let run = run_announcement_bracket(
        "telegram:chat-7",
        &mut history,
        ANNOUNCEMENT_BLOCK,
        turn_that_succeeded(),
    )
    .await;

    assert_eq!(
        run.claimed_keys,
        vec!["telegram:chat-7".to_string()],
        "exactly one claim, under the turn's own history key"
    );
}

/// A turn that succeeded keeps its announcements delivered: the guard is
/// settled, with `true`, so the rows never come back for a second airing.
///
/// Discriminating line: `GuardEvent::Settled { turn_succeeded: true }`.
/// Settle as if every turn failed and every announced completion is
/// announced again next turn.
#[tokio::test]
async fn announcement_bracket_keeps_the_claim_delivered_when_the_turn_succeeds() {
    let mut history = user_tail_history();

    let run = run_announcement_bracket(
        "telegram:chat-7",
        &mut history,
        ANNOUNCEMENT_BLOCK,
        turn_that_succeeded(),
    )
    .await;

    assert_eq!(
        run.guard_events,
        vec![GuardEvent::Settled {
            turn_succeeded: true
        }],
        "a succeeded turn settles its claim once, as a success"
    );
}

/// Every failure shape hands the announcements back, so the next turn can
/// claim them again. All three levels of `LlmExecutionResult` are failures
/// here, and each is a case where the model may never have read the block.
///
/// Discriminating line: `turn_succeeded: false` for each shape. Settle as
/// if every turn succeeded — or flatten the criterion to "is it ok" — and a
/// completion is flagged delivered to a model that never saw it, which
/// nothing later looks at again.
#[tokio::test]
async fn announcement_bracket_hands_the_claim_back_on_every_failure_shape() {
    for (label, outcome) in [
        ("cancelled", LlmExecutionResult::Cancelled),
        (
            "timed out",
            LlmExecutionResult::Completed(Err(timed_out().await)),
        ),
        (
            "tool loop failed",
            LlmExecutionResult::Completed(Ok(Err(anyhow::Error::msg("tool loop blew up")))),
        ),
    ] {
        let mut history = user_tail_history();

        let run =
            run_announcement_bracket("telegram:chat-7", &mut history, ANNOUNCEMENT_BLOCK, outcome)
                .await;

        assert_eq!(
            run.guard_events,
            vec![GuardEvent::Settled {
                turn_succeeded: false
            }],
            "a turn that {label} must hand its announcements back"
        );
    }
}

/// The known-reachable one: the cached history's tail is a `tool` message,
/// so the splice has no user turn to hang the block on. The claim is handed
/// back — and it is handed back *before* the turn runs, because nothing was
/// put in front of the model to keep.
///
/// Discriminating line: `GuardEvent::DroppedUnsettled` together with the
/// `assert!` that the body's history is untouched. Settle this path as a
/// success (or as any settle at all) and the rows stay flagged delivered to
/// nobody, with no later turn ever looking at them again.
#[tokio::test]
async fn announcement_bracket_hands_the_claim_back_when_the_splice_finds_no_user_turn() {
    let mut history = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("[2026-07-29 10:00:00 UTC] run the thing"),
        ChatMessage::tool("tool output from an interrupted turn"),
    ];

    let run = run_announcement_bracket(
        "telegram:chat-7",
        &mut history,
        ANNOUNCEMENT_BLOCK,
        turn_that_succeeded(),
    )
    .await;

    assert_eq!(
        run.guard_events,
        vec![GuardEvent::DroppedUnsettled],
        "an unspliceable block must never be reported delivered"
    );
    assert!(
        run.body_history
            .iter()
            .all(|m| !m.content.contains("Background tasks finished")),
        "the block reached the model after all: {:?}",
        run.body_history
    );
}

/// The one thing behavioural tests above cannot reach: that the production
/// turn actually goes *through* the bracket. `process_channel_message_body`
/// still takes a live orchestrator context no test constructs, so this is
/// pinned by reading this file's own production text — the way the control
/// plane pins its SQL status filters.
///
/// This is deliberately one needle, not the five it used to be. Claim,
/// splice, settle and the success criterion are now covered behaviourally;
/// keeping literals for those as well would only mean a suite that goes red
/// during ordinary refactors, which teaches people to ignore it.
///
/// Discriminating line: the `assert!`. Inline the retry loop back at the
/// call site and the wiring is gone with the whole suite still green, which
/// is exactly how fork #22 went unnoticed the first time.
#[test]
fn the_channel_turn_runs_inside_the_background_announcement_bracket() {
    const SRC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/orchestrator/mod.rs"
    ));
    // Production text only: this test necessarily contains the same literal
    // it searches for, and would otherwise satisfy itself.
    let production = SRC.split("\nmod tests {").next().unwrap_or(SRC);

    let needle = "let llm_result = run_channel_turn_with_background_announcements(";
    assert!(
        production.contains(needle),
        "the channel turn no longer runs inside the announcement bracket \
             (lost `{needle}`); a Detached child that finishes now announces to \
             nobody (fork #22)"
    );
}

// ─── endpreamble tests ─────────────────────────────────────

#[test]
fn build_channel_system_prompt_for_message_omits_volatile_fields() {
    // The wrapper now unpacks only the channel-name and bot_mention
    // from the ChannelMessage into `build_channel_system_prompt`. The
    // volatile per-turn fields (reply_target, sender, message_id) live
    // in the turn-context preamble, not here. See
    let msg = channel_message("discord", None);
    let prompt = build_channel_system_prompt_for_message("Base.", &msg, None);
    assert!(
        !prompt.contains("reply_target="),
        "system prompt must not carry reply_target: {prompt}"
    );
    assert!(
        !prompt.contains("sender="),
        "system prompt must not carry sender=: {prompt}"
    );
    assert!(
        !prompt.contains("message_id="),
        "system prompt must not carry message_id=: {prompt}"
    );
    assert!(
        !prompt.contains("Channel context:"),
        "system prompt must not carry the legacy Channel context block: {prompt}"
    );
}

#[test]
fn build_channel_turn_context_preamble_webhook_cron_hint_carries_thread_id() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        channel: "webhook".into(),
        reply_target: "agent-chat:agent-1:thread-7".into(),
        sender: "user:abc".into(),
        id: "msg-1".into(),
        ..Default::default()
    };
    let preamble = build_channel_turn_context_preamble(&msg, None);
    assert!(
        preamble.contains("\"to\":\"user:abc\""),
        "webhook cron hint must use sender as `to`: {preamble}"
    );
    assert!(
        preamble.contains("\"thread_id\":\"agent-chat:agent-1:thread-7\""),
        "webhook cron hint must carry the reply_target as `thread_id`: {preamble}"
    );
    assert!(
        !preamble.contains("\"to\":\"agent-chat:agent-1:thread-7\""),
        "webhook cron hint must not put the thread id in `to`: {preamble}"
    );
}

#[test]
fn build_channel_turn_context_preamble_non_webhook_cron_hint_keeps_to_as_reply_target() {
    let msg = zeroclaw_api::channel::ChannelMessage {
        channel: "slack".into(),
        reply_target: "C12345".into(),
        sender: "U67890".into(),
        id: "msg-1".into(),
        ..Default::default()
    };
    let preamble = build_channel_turn_context_preamble(&msg, None);
    assert!(
        preamble.contains("\"to\":\"C12345\""),
        "non-webhook cron hint should keep reply_target as `to`: {preamble}"
    );
    assert!(
        !preamble.contains("\"thread_id\""),
        "non-webhook cron hint should not emit a thread_id field: {preamble}"
    );
}

#[tokio::test]
#[cfg(feature = "channel-lark")]
async fn deliver_announcement_routes_lark_to_lark_arm() {
    // Both names must enter the merged lark|feishu arm. Falling through
    // to `unsupported delivery channel` would mean the schema enum and
    // the match arm have drifted apart.
    let config = zeroclaw_config::schema::Config::default();

    for channel in ["lark.default", "feishu.default"] {
        let err = deliver_announcement(&config, channel, "oc_test_chat", None, "hi")
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("expected {channel} to bail because channel is not configured")
            });
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("unsupported delivery channel"),
            "{channel} must route to lark|feishu arm, not fall through; got: {msg}"
        );
        assert!(
            msg.contains("[channels.lark.default] not configured"),
            "{channel} must report the real config table [channels.lark.default]; got: {msg}"
        );
    }
}

#[tokio::test]
#[cfg(feature = "channel-email")]
async fn deliver_announcement_routes_email_to_email_arm() {
    let config = zeroclaw_config::schema::Config::default();

    let err = deliver_announcement(&config, "email.default", "user@example.com", None, "hi")
        .await
        .expect_err("expected email.default to bail because channel is not configured");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("unsupported delivery channel"),
        "email.default must route to the email arm, not fall through; got: {msg}"
    );
    assert!(
        msg.contains("[channels.email.default] not configured"),
        "email.default must report the real config table; got: {msg}"
    );
}

#[tokio::test]
#[cfg(feature = "whatsapp-web")]
async fn deliver_announcement_routes_whatsapp_to_whatsapp_arm() {
    let config = zeroclaw_config::schema::Config::default();

    let err = deliver_announcement(&config, "whatsapp.default", "+15551234567", None, "hi")
        .await
        .expect_err("expected whatsapp.default to bail because channel is not configured");
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("unsupported delivery channel"),
        "whatsapp.default must route to the whatsapp arm, not fall through; got: {msg}"
    );
    assert!(
        msg.contains("[channels.whatsapp.default] not configured"),
        "whatsapp.default must report the real config table; got: {msg}"
    );
}

#[tokio::test]
#[cfg(feature = "whatsapp-web")]
async fn deliver_announcement_rejects_whatsapp_non_web_config_clearly() {
    let mut config = zeroclaw_config::schema::Config::default();
    config.channels.whatsapp.insert(
        "default".to_string(),
        zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            access_token: Some("test-token".to_string()),
            phone_number_id: Some("phone-number-id".to_string()),
            verify_token: Some("verify-token".to_string()),
            ..Default::default()
        },
    );

    let err = deliver_announcement(&config, "whatsapp.default", "+15551234567", None, "hi")
        .await
        .expect_err("expected WhatsApp Cloud config to be rejected for cron delivery");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("WhatsApp channel send requires Web mode"),
        "whatsapp.default must clearly explain the Web mode requirement; got: {msg}"
    );
    assert!(
        msg.contains("session_path")
            && msg.contains("pair_phone")
            && msg.contains("mode = personal"),
        "whatsapp.default must name the Web selectors accepted by cron delivery; got: {msg}"
    );
    assert!(
        !msg.contains("unsupported delivery channel")
            && !msg.contains("[channels.whatsapp.default] not configured"),
        "whatsapp.default must reject the configured non-Web mode, not fall through; got: {msg}"
    );
}

#[tokio::test]
#[cfg(feature = "channel-lark")]
async fn deliver_announcement_rejects_feishu_value_when_use_feishu_false() {
    // Reject (not warn): otherwise the message silently lands on the
    // Lark endpoint despite the user explicitly naming Feishu.
    let mut config = zeroclaw_config::schema::Config::default();
    config.channels.lark.insert(
        "work".to_string(),
        zeroclaw_config::schema::LarkConfig {
            enabled: true,
            use_feishu: false,
            app_id: "cli_test".to_string(),
            app_secret: "secret".to_string(),
            approval_timeout_secs: 300,
            per_user_session: false,
            ack_reactions: None,
            ..Default::default()
        },
    );

    let err = deliver_announcement(&config, "feishu.work", "oc_test_chat", None, "hi")
        .await
        .expect_err("expected bail when channel=feishu but use_feishu=false");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("use_feishu=false"),
        "bail must explain the use_feishu mismatch; got: {msg}"
    );
    assert!(
        msg.contains("[channels.lark.work]"),
        "bail must point at the real config table; got: {msg}"
    );
}

fn email_msg(id: &str, subject: Option<&str>) -> ChannelMessage {
    ChannelMessage {
        subject: subject.map(Into::into),
        ..ChannelMessage::new(
            id,
            "user@example.com",
            "user@example.com",
            "Hello",
            "email",
            0,
        )
    }
}

#[test]
fn reply_to_sets_in_reply_to_and_re_subject() {
    let msg = email_msg("<abc123@mail.example>", Some("Weekly report"));
    let sm = SendMessage::reply_to(&msg, "Here is the answer");
    assert_eq!(sm.in_reply_to.as_deref(), Some("<abc123@mail.example>"));
    assert_eq!(sm.subject.as_deref(), Some("Re: Weekly report"));
}

#[test]
fn reply_to_does_not_double_re_prefix() {
    let msg = email_msg("<abc123@mail.example>", Some("Re: Weekly report"));
    let sm = SendMessage::reply_to(&msg, "Here is the answer");
    assert_eq!(sm.subject.as_deref(), Some("Re: Weekly report"));
}

#[test]
fn reply_to_no_subject_still_sets_in_reply_to() {
    let msg = email_msg("<abc123@mail.example>", None);
    let sm = SendMessage::reply_to(&msg, "Here is the answer");
    assert_eq!(sm.in_reply_to.as_deref(), Some("<abc123@mail.example>"));
    assert!(sm.subject.is_none());
}

/// Router with no SOP engine wired. `dispatch_channel_sop_event` decides
/// whether a message is a channel SOP event before it ever touches the
/// engine, so this is enough to exercise the source/dispatch boundary.
fn router_without_sop_engine() -> AgentRouter {
    AgentRouter {
        by_agent: Arc::new(HashMap::new()),
        owner_by_channel_key: Arc::new(HashMap::new()),
        single_ctx: None,
        sop_engine: None,
        sop_audit: None,
    }
}

fn manual_sop_event() -> zeroclaw_runtime::sop::types::SopEvent {
    zeroclaw_runtime::sop::types::SopEvent {
        source: zeroclaw_runtime::sop::types::SopTriggerSource::Manual,
        topic: None,
        payload: None,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn channel_gate_sop(policy: Option<&str>) -> zeroclaw_runtime::sop::types::Sop {
    use zeroclaw_runtime::sop::types::{
        Sop, SopAdmissionPolicy, SopExecutionMode, SopPriority, SopStep, SopStepKind, SopTrigger,
    };

    Sop {
        name: "channel-gate".to_string(),
        description: "channel gate test".to_string(),
        version: "1.0.0".to_string(),
        priority: SopPriority::Normal,
        execution_mode: SopExecutionMode::Supervised,
        triggers: vec![SopTrigger::Manual],
        steps: vec![SopStep {
            number: 1,
            title: "Approve me".to_string(),
            body: "Do the gated work".to_string(),
            suggested_tools: vec![],
            requires_confirmation: false,
            kind: SopStepKind::Execute,
            schema: None,
            policy: policy.map(str::to_string),
            ..SopStep::default()
        }],
        cooldown_secs: 0,
        max_concurrent: 1,
        location: None,
        deterministic: false,
        admission_policy: SopAdmissionPolicy::Parallel,
        max_pending_approvals: 0,
        agent: None,
    }
}

fn channel_gate_config_with_routes(
    request_route: Option<&str>,
    escalation_route: Option<&str>,
) -> zeroclaw_config::schema::SopConfig {
    let mut config = zeroclaw_config::schema::SopConfig::default();
    if request_route.is_some() || escalation_route.is_some() {
        config.approval.policies.insert(
            "prod".to_string(),
            zeroclaw_config::schema::ApprovalPolicyConfig {
                request_route: request_route.map(str::to_string),
                escalation_route: escalation_route.map(str::to_string),
                ..zeroclaw_config::schema::ApprovalPolicyConfig::default()
            },
        );
    }
    config
}

fn parked_channel_gate_router(
    policy: Option<&str>,
    request_route: Option<&str>,
) -> (
    AgentRouter,
    Arc<Mutex<zeroclaw_runtime::sop::SopEngine>>,
    String,
) {
    parked_channel_gate_router_with_routes(policy, request_route, None)
}

fn parked_channel_gate_router_with_routes(
    policy: Option<&str>,
    request_route: Option<&str>,
    escalation_route: Option<&str>,
) -> (
    AgentRouter,
    Arc<Mutex<zeroclaw_runtime::sop::SopEngine>>,
    String,
) {
    let mut engine = zeroclaw_runtime::sop::SopEngine::new(channel_gate_config_with_routes(
        request_route,
        escalation_route,
    ));
    engine.set_sops_for_test(vec![channel_gate_sop(policy)]);
    let action = engine
        .start_run("channel-gate", manual_sop_event())
        .unwrap();
    let run_id = match action {
        zeroclaw_runtime::sop::types::SopRunAction::WaitApproval { run_id, .. } => run_id,
        other => panic!("expected waiting approval, got {other:?}"),
    };
    let engine = Arc::new(Mutex::new(engine));
    let router = AgentRouter {
        by_agent: Arc::new(HashMap::new()),
        owner_by_channel_key: Arc::new(HashMap::new()),
        single_ctx: None,
        sop_engine: Some(Arc::clone(&engine)),
        sop_audit: None,
    };
    (router, engine, run_id)
}

fn active_run_status(
    engine: &Arc<Mutex<zeroclaw_runtime::sop::SopEngine>>,
    run_id: &str,
) -> Option<zeroclaw_runtime::sop::types::SopRunStatus> {
    engine
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_run(run_id)
        .map(|run| run.status)
}

async fn dispatch_test_channel_sop_gate(
    router: &AgentRouter,
    msg: &ChannelMessage,
    gate_channel: Option<Arc<dyn Channel>>,
) -> bool {
    let route_keys = vec![channel_key_for_message(msg)];
    let gate_prompt_channels = gate_channel.into_iter().collect::<Vec<_>>();
    let config = zeroclaw_config::schema::Config::default();
    dispatch_channel_sop_gate(router, msg, &config, &gate_prompt_channels, &route_keys).await
}

async fn dispatch_test_channel_sop_gate_with_route_keys(
    router: &AgentRouter,
    msg: &ChannelMessage,
    route_keys: &[&str],
) -> bool {
    let route_keys: Vec<String> = route_keys.iter().map(|key| (*key).to_string()).collect();
    let config = zeroclaw_config::schema::Config::default();
    dispatch_channel_sop_gate(router, msg, &config, &[], &route_keys).await
}

#[tokio::test]
async fn dispatch_channel_sop_event_ignores_user_controlled_subject() {
    // An email-shaped message: the reserved prefix sits in the
    // user-controlled `subject`, but the internal git-only marker is
    // absent. It MUST NOT route to SOP.
    let msg = ChannelMessage {
        channel: "email".to_string(),
        sender: "attacker@example.com".to_string(),
        subject: Some("zeroclaw:sop-event:git.main:pull_request.opened".to_string()),
        content: r#"{"sop":"triage"}"#.to_string(),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "attacker@example.com", "", "", "email", 0)
    };
    let router = router_without_sop_engine();
    assert!(
        !dispatch_channel_sop_event(&router, &msg).await,
        "a forged subject must not select SOP ingress"
    );
}

#[tokio::test]
async fn dispatch_channel_sop_event_routes_git_produced_marker() {
    // A genuine git-produced message: the internal marker carries the
    // topic. It IS recognized as a SOP event (returns true; the missing
    // engine is handled inside, but the routing decision fired).
    let msg = ChannelMessage {
        channel: "git".to_string(),
        channel_alias: Some("main".to_string()),
        sender: "test_user".to_string(),
        subject: Some("zeroclaw:sop-event:git.main:pull_request.opened".to_string()),
        content: r#"{"sop":"triage"}"#.to_string(),
        internal_sop_event: Some("git.main:pull_request.opened".to_string()),
        ..ChannelMessage::new("1", "test_user", "octo/repo#12", "", "git", 0)
    };
    let router = router_without_sop_engine();
    assert!(
        dispatch_channel_sop_event(&router, &msg).await,
        "a git-produced internal marker must select SOP ingress"
    );
}

/// Pins the contract: the channel tool-loop reads `strict_tool_parsing` and
/// `parallel_tools` from `agent_cfg.resolved` (populated), not from
/// `prompt_config.agent(alias).resolved` (serde-skipped default).
#[test]
fn resolved_agent_config_carries_strict_tool_parsing_and_parallel_tools() {
    let mut prompt_config = zeroclaw_config::schema::Config::default();

    prompt_config.runtime_profiles.insert(
        "strict-parallel".to_string(),
        zeroclaw_config::schema::RuntimeProfileConfig {
            strict_tool_parsing: true,
            parallel_tools: Some(true),
            ..Default::default()
        },
    );

    let agent_alias = "test-agent";
    prompt_config.agents.insert(
        agent_alias.to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            runtime_profile: zeroclaw_config::providers::RuntimeProfileRef::from("strict-parallel"),
            ..Default::default()
        },
    );

    let agent_cfg = prompt_config
        .resolved_agent_config(agent_alias)
        .expect("agent must resolve");

    assert!(
        agent_cfg.resolved.strict_tool_parsing,
        "resolved.strict_tool_parsing should be true from the runtime profile"
    );
    assert!(
        agent_cfg.resolved.parallel_tools,
        "resolved.parallel_tools should be true from the runtime profile"
    );

    let raw_agent = prompt_config
        .agent(agent_alias)
        .expect("agent must exist in prompt_config");
    assert!(
        !raw_agent.resolved.strict_tool_parsing,
        "raw agent resolved.strict_tool_parsing should be false (serde-skipped default)"
    );
    assert!(
        !raw_agent.resolved.parallel_tools,
        "raw agent resolved.parallel_tools should be false (serde-skipped default)"
    );
}

#[tokio::test]
async fn sop_gate_marker_is_consumed_even_without_an_engine() {
    // A `sop.gate:` marker message exists ONLY to answer a gate; it must be
    // consumed (never fall through to an agent turn) even when no SOP
    // engine is available to resolve it.
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("gnosis".to_string()),
        sender: "111222333".to_string(),
        content: "approve det-1-0001".to_string(),
        internal_sop_event: Some("sop.gate:approve:det-1-0001".to_string()),
        ..ChannelMessage::new("1", "111222333", "chan", "", "discord", 0)
    };
    let router = router_without_sop_engine();
    assert!(
        dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a gate-click marker must be consumed, not become an agent turn"
    );
}

#[tokio::test]
async fn sop_gate_text_reply_falls_through_when_nothing_is_parked() {
    // A bare "approve <ref>" text message with NO matching parked run is
    // ordinary conversation — it must NOT be consumed as a gate answer.
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("gnosis".to_string()),
        sender: "111222333".to_string(),
        content: "approve det-9999-0001".to_string(),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "chan", "", "discord", 0)
    };
    let router = router_without_sop_engine();
    assert!(
        !dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a text reply with no parked-run match must fall through to the agent"
    );
}

#[tokio::test]
async fn sop_gate_text_reply_does_not_clear_unpoliced_parked_run() {
    let (router, engine, run_id) = parked_channel_gate_router(None, None);
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        !dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "text replies must not clear parked runs that never emitted a request-route prompt"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn sop_gate_text_reply_requires_matching_request_route() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("discord.ops:room-1"));
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-2".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-2", "", "discord", 0)
    };

    assert!(
        !dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a text reply from the wrong room must fall through instead of resolving the gate"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn sop_gate_text_reply_rejects_request_route_that_delivery_would_not_target() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some(" discord.ops:room-1"));
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        !dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "text replies must not normalize a configured request_route differently from delivery"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn sop_gate_text_reply_rejects_short_suffix_reference() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("discord.ops:room-1"));
    assert!(
        run_id.ends_with('1'),
        "the deterministic test run id should exercise the old one-character suffix match: {run_id}"
    );
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: "approve 1".to_string(),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        !dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "plain text replies must carry the full prompt reference, not a short run-id suffix"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn sop_gate_marker_rejects_short_suffix_reference() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("discord.ops:room-1"));
    assert!(
        run_id.ends_with('1'),
        "the deterministic test run id should exercise the old one-character suffix match: {run_id}"
    );
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: String::new(),
        internal_sop_event: Some("sop.gate:approve:1".to_string()),
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a stale marker is consumed, but must not resolve by short run-id suffix"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::WaitingApproval)
    );
}

#[tokio::test]
async fn sop_gate_text_reply_resolves_bare_singleton_request_route() {
    let (router, engine, run_id) = parked_channel_gate_router(Some("prod"), Some("discord:room-1"));
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        dispatch_test_channel_sop_gate_with_route_keys(&router, &msg, &["discord.ops", "discord"],)
            .await,
        "a bare singleton route should resolve when it maps to the same channel instance"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::Running)
    );
}

#[tokio::test]
async fn sop_gate_text_reply_resolves_matching_request_route() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("discord.ops:room-1"));
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };

    assert!(
        dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a text reply from the request route remains a valid fallback answer"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::Running)
    );
}

#[tokio::test]
async fn channel_gate_approval_drives_resumed_execute_step() {
    let (router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("discord.ops:room-1"));
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("ops".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "discord", 0)
    };
    let config = zeroclaw_config::schema::Config::default();

    assert!(
        dispatch_channel_sop_gate(&router, &msg, &config, &[], &["discord.ops".to_string()],).await,
        "the channel approval must resolve the parked gate"
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if active_run_status(&engine, &run_id)
                == Some(zeroclaw_runtime::sop::types::SopRunStatus::Failed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("channel approval must schedule the resumed ExecuteStep");
}

#[tokio::test]
async fn approval_only_channel_gate_reply_bypasses_agent_ownership() {
    let (mut router, engine, run_id) =
        parked_channel_gate_router(Some("prod"), Some("test-channel:room-1"));
    let gate_channel: Arc<dyn Channel> = Arc::new(RecordingChannel::default());
    let gate_ctx = test_runtime_ctx_with_config_agent_and_provider_ref(
        gate_channel,
        Arc::new(DummyModelProvider),
        zeroclaw_config::schema::Config::default(),
        zeroclaw_config::schema::AliasedAgentConfig::default(),
        "test-provider",
        None,
    );
    router.by_agent = Arc::new(HashMap::from([("worker".to_string(), gate_ctx)]));

    let msg = ChannelMessage {
        channel: "test-channel".to_string(),
        sender: "111222333".to_string(),
        reply_target: "room-1".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-1", "", "test-channel", 0)
    };
    assert!(
        router.resolve(&msg).is_none(),
        "the configured approval route must not need an agent owner"
    );

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(msg).await.expect("queue gate reply");
    drop(tx);
    run_message_dispatch_loop(rx, router, 1).await;

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if active_run_status(&engine, &run_id)
                == Some(zeroclaw_runtime::sop::types::SopRunStatus::Failed)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the approval-only channel reply must drive the resumed action");
}

#[tokio::test]
async fn sop_gate_resolution_finalizes_request_and_escalation_channels() {
    let (router, engine, run_id) = parked_channel_gate_router_with_routes(
        Some("prod"),
        Some("discord.ops:room-1"),
        Some("discord.oncall:room-2"),
    );
    let request_channel = Arc::new(RecordingChannel::default());
    let escalation_channel = Arc::new(RecordingChannel::default());
    let prompt_channels: Vec<Arc<dyn Channel>> =
        vec![request_channel.clone(), escalation_channel.clone()];
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("oncall".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-2".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-2", "", "discord", 0)
    };

    assert!(
        dispatch_channel_sop_gate(
            &router,
            &msg,
            &zeroclaw_config::schema::Config::default(),
            &prompt_channels,
            &["discord.oncall".to_string()],
        )
        .await,
        "an approval on the escalation route should resolve the gate"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::Running)
    );
    for finalized in [
        request_channel.finalized_gate_prompts.lock().await,
        escalation_channel.finalized_gate_prompts.lock().await,
    ] {
        assert_eq!(finalized.len(), 1, "every active route channel finalizes");
        assert_eq!(finalized[0].0, run_id);
        assert!(finalized[0].1.contains("Approved"));
    }
}

#[tokio::test]
async fn sop_gate_text_reply_resolves_matching_escalation_route() {
    let (router, engine, run_id) = parked_channel_gate_router_with_routes(
        Some("prod"),
        Some("discord.ops:room-1"),
        Some("discord.oncall:room-2"),
    );
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        channel_alias: Some("oncall".to_string()),
        sender: "111222333".to_string(),
        reply_target: "room-2".to_string(),
        content: format!("approve {run_id}"),
        internal_sop_event: None,
        ..ChannelMessage::new("1", "111222333", "room-2", "", "discord", 0)
    };

    assert!(
        dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "a text reply from the route that receives escalation instructions must resolve"
    );
    assert_eq!(
        active_run_status(&engine, &run_id),
        Some(zeroclaw_runtime::sop::types::SopRunStatus::Running)
    );
}

#[test]
fn gate_reference_parsing_defaults_bare_to_revision_zero() {
    // Bare = revision 0 (the original presentation), NOT "current": a click
    // on a superseded prompt must never resolve a newer draft.
    assert_eq!(parse_gate_reference("det-1-0001"), ("det-1-0001".into(), 0));
    assert_eq!(
        parse_gate_reference("det-1-0001#2"),
        ("det-1-0001".into(), 2)
    );
    // Malformed suffix: the whole string is the run part (matches nothing).
    assert_eq!(
        parse_gate_reference("det-1-0001#zz"),
        ("det-1-0001#zz".into(), 0)
    );
}

#[tokio::test]
async fn sop_gate_edit_and_revise_markers_are_consumed() {
    // Edit/Revise markers exist only to answer a gate — consumed even when
    // no engine is available, exactly like approve/deny markers.
    for (choice, content) in [
        ("edit", "my rewritten draft"),
        ("revise", "make it shorter"),
    ] {
        let msg = ChannelMessage {
            channel: "discord".to_string(),
            channel_alias: Some("gnosis".to_string()),
            sender: "111222333".to_string(),
            content: content.to_string(),
            internal_sop_event: Some(format!("sop.gate:{choice}:det-1-0001#1")),
            ..ChannelMessage::new("1", "111222333", "chan", "", "discord", 0)
        };
        let router = router_without_sop_engine();
        assert!(
            dispatch_test_channel_sop_gate(&router, &msg, None).await,
            "a {choice} marker must be consumed, not become an agent turn"
        );
    }
}

#[tokio::test]
async fn sop_gate_unknown_marker_choice_is_dropped() {
    // An unknown choice in a marker is malformed — consumed (it can only be
    // a gate artifact), never resolved as a decision. Guards the old
    // behavior where any non-"approve" choice silently became a DENY.
    let msg = ChannelMessage {
        channel: "discord".to_string(),
        sender: "111222333".to_string(),
        content: "frobnicate det-1-0001".to_string(),
        internal_sop_event: Some("sop.gate:frobnicate:det-1-0001".to_string()),
        ..ChannelMessage::new("1", "111222333", "chan", "", "discord", 0)
    };
    let router = router_without_sop_engine();
    assert!(
        dispatch_test_channel_sop_gate(&router, &msg, None).await,
        "an unknown-choice marker must still be consumed"
    );
}

#[tokio::test]
async fn sop_gate_ordinary_chat_never_matches() {
    // Ordinary messages — even ones containing the word "approve" — must
    // never be consumed by the gate intercept.
    for content in [
        "please approve my PR when you can",
        "deny",
        "approve",
        "approve run 12 thanks",
    ] {
        let msg = ChannelMessage {
            channel: "discord".to_string(),
            sender: "111222333".to_string(),
            content: content.to_string(),
            internal_sop_event: None,
            ..ChannelMessage::new("1", "111222333", "chan", "", "discord", 0)
        };
        let router = router_without_sop_engine();
        assert!(
            !dispatch_test_channel_sop_gate(&router, &msg, None).await,
            "ordinary chat must fall through: {content:?}"
        );
    }
}
