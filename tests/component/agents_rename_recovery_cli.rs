//! `zeroclaw agents` against the agent rename recovery contract the CLI, the
//! gateway, and RPC share: a committed rename resumes instead of reporting the
//! old alias missing, the command fails until every follower has converged,
//! and an unfinished rename keeps its old alias retired and its target in
//! place across surfaces.
//!
//! The binary and the public gateway handler are the only surfaces these tests
//! drive. The stores are seeded and read back through their public
//! constructors.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use zeroclaw_config::agent_recovery_journal::AgentRecoveryJournal;
use zeroclaw_config::alias_refs::{self, AliasKind};
use zeroclaw_config::schema::{AliasedAgentConfig, Config};
use zeroclaw_memory::{Memory, SqliteMemory};

fn fixture(dir: &Path) -> Config {
    let mut config = Config {
        config_path: dir.join("config.toml"),
        data_dir: dir.join("data"),
        // The assertions read English text whatever the host locale is.
        locale: Some("en".to_string()),
        ..Config::default()
    };
    std::fs::create_dir_all(&config.data_dir).unwrap();
    config.agents.insert(
        "agent_a".into(),
        AliasedAgentConfig {
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
    config
        .risk_profiles
        .entry("default".into())
        .or_default()
        .allowed_commands = vec!["echo".into()];
    config.runtime_profiles.entry("default".into()).or_default();
    config
}

fn run_zeroclaw(config_dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env("RUST_LOG", "off")
        .arg("--config-dir")
        .arg(config_dir)
        .args(args);
    command.output().expect("spawn zeroclaw")
}

fn run_agents(config_dir: &Path, args: &[&str]) -> Output {
    let mut agents_args = vec!["agents"];
    agents_args.extend_from_slice(args);
    run_zeroclaw(config_dir, &agents_args)
}

fn text_of(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The agent aliases the saved config declares.
fn configured_agents(config_dir: &Path) -> Vec<String> {
    let saved = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
    let config: Config = toml::from_str(&saved).unwrap();
    config.agents.keys().cloned().collect()
}

async fn seed_followers(config: &Config) -> PathBuf {
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"owned").unwrap();
    zeroclaw_runtime::cron::add_job(config, "agent_a", "* * * * *", "echo hi").unwrap();
    {
        let memory = SqliteMemory::new("agent_a", &config.data_dir).unwrap();
        memory.ensure_agent_uuid("agent_a").await.unwrap();
    }
    {
        let sessions = zeroclaw_infra::make_session_backend(&config.data_dir, "sqlite").unwrap();
        sessions
            .set_session_agent_alias("sess-1", "agent_a")
            .unwrap();
    }
    {
        let acp =
            zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir).unwrap();
        acp.create_session("acp-1", "agent_a", old_ws.to_string_lossy().as_ref(), None)
            .unwrap();
    }
    old_ws
}

async fn assert_followers_on(config: &Config, alias: &str, present: bool) {
    let memory = SqliteMemory::new("probe", &config.data_dir).unwrap();
    let memory_count = memory.count_agent(alias).await.unwrap();
    let cron_count = zeroclaw_runtime::cron::list_jobs_by_agent(config, alias)
        .unwrap()
        .len();
    let acp = zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir).unwrap();
    let acp_count = acp.list_sessions_by_agent(alias).unwrap().len();
    let sessions = zeroclaw_infra::make_session_backend(&config.data_dir, "sqlite").unwrap();
    let session_count = sessions.count_agent_attribution(alias).unwrap();
    if present {
        assert_eq!(memory_count, 1, "memory rows for {alias}");
        assert_eq!(cron_count, 1, "cron jobs for {alias}");
        assert_eq!(acp_count, 1, "acp sessions for {alias}");
        assert_eq!(session_count, 1, "session attribution for {alias}");
    } else {
        assert_eq!(memory_count, 0, "memory rows for {alias}");
        assert_eq!(cron_count, 0, "cron jobs for {alias}");
        assert_eq!(acp_count, 0, "acp sessions for {alias}");
        assert_eq!(session_count, 0, "session attribution for {alias}");
    }
}

/// Start a rename of `agent_a` to `agent_b` that cannot finish: a file stands
/// where the new workspace's directory must go, so the workspace move fails
/// after the config commit. Returns the unmoved workspace and the blocker.
async fn leave_rename_unfinished(dir: &Path) -> (PathBuf, PathBuf) {
    let config = fixture(dir);
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"owned").unwrap();
    config.save().await.unwrap();
    let blocker = dir.join("agents").join("agent_b");
    std::fs::write(&blocker, b"x").unwrap();

    let renamed = run_agents(dir, &["rename", "agent_a", "agent_b"]);
    let renamed_text = text_of(&renamed);
    assert!(
        !renamed.status.success(),
        "a blocked workspace move leaves the rename unfinished: {renamed_text}"
    );
    assert!(
        renamed_text.contains("zeroclaw agents rename agent_a agent_b"),
        "an unfinished rename names the command that finishes it: {renamed_text}"
    );
    (old_ws, blocker)
}

#[tokio::test]
async fn committed_rename_cli_resumes_followers_then_allows_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path());
    let old_ws = seed_followers(&config).await;
    let new_ws = {
        let mut preview = config.clone();
        alias_refs::rename_with_cascade(&mut preview, &AliasKind::Agent, "agent_a", "agent_b")
            .unwrap();
        preview.agent_workspace_dir("agent_b")
    };
    alias_refs::rename_with_cascade(&mut config, &AliasKind::Agent, "agent_a", "agent_b").unwrap();
    config.save().await.unwrap();

    let first = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let first_text = text_of(&first);
    assert!(
        first.status.success(),
        "a committed rename with follower residue must resume, not report the alias missing: {first_text}"
    );
    assert!(new_ws.join("marker").is_file(), "workspace marker moved");
    assert!(!old_ws.exists(), "old workspace removed");
    assert_followers_on(&config, "agent_b", true).await;
    assert_followers_on(&config, "agent_a", false).await;

    let second = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let second_text = text_of(&second);
    assert!(
        !second.status.success(),
        "recovery is cleared after convergence: {second_text}"
    );
    assert!(
        second_text.contains("is not configured"),
        "a finished rename is not configured anymore: {second_text}"
    );

    let created = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        created.status.success(),
        "reuse is allowed after convergence: {}",
        text_of(&created)
    );
    assert_followers_on(&config, "agent_a", false).await;
    assert_followers_on(&config, "agent_b", true).await;
}

#[tokio::test]
async fn committed_rename_cli_unreadable_cron_stays_retryable_and_blocks_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path());
    alias_refs::rename_with_cascade(&mut config, &AliasKind::Agent, "agent_a", "agent_b").unwrap();
    config.save().await.unwrap();
    std::fs::create_dir_all(config.data_dir.join("cron").join("jobs.db")).unwrap();

    let renamed = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let renamed_text = text_of(&renamed);
    assert!(!renamed.status.success(), "{renamed_text}");
    assert!(
        !renamed_text.contains("is not configured") && !renamed_text.contains("alias not found"),
        "an unreadable cron store is not an absent alias: {renamed_text}"
    );

    let created = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        !created.status.success(),
        "the retired alias cannot be recreated while recovery is outstanding: {}",
        text_of(&created)
    );
}

#[tokio::test]
async fn committed_rename_cli_blocked_workspace_survives_out_of_band_wipe() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fixture(tmp.path());
    let old_ws = seed_followers(&config).await;
    config.save().await.unwrap();
    let blocker = tmp.path().join("agents").join("agent_b");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"x").unwrap();

    let first = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let first_text = text_of(&first);
    assert!(
        !first.status.success(),
        "a failed follower converge must be retryable: {first_text}"
    );
    assert!(
        !first_text.contains("is not configured"),
        "the source alias was configured when the rename started: {first_text}"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "marker stays when the move fails"
    );

    let blocked = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        !blocked.status.success(),
        "open recovery blocks the retired alias: {}",
        text_of(&blocked)
    );
    let other = run_agents(tmp.path(), &["create", "agent_c"]);
    assert!(
        other.status.success(),
        "another alias can still be created: {}",
        text_of(&other)
    );

    std::fs::remove_file(&blocker).unwrap();
    std::fs::remove_dir_all(&old_ws).unwrap();
    let still = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        !still.status.success(),
        "deleting residue outside the rename does not finish recovery: {}",
        text_of(&still)
    );

    let second = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    assert!(
        second.status.success(),
        "retry converges and clears recovery: {}",
        text_of(&second)
    );
    let freed = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        freed.status.success(),
        "reuse works after a clean converge: {}",
        text_of(&freed)
    );
    assert_followers_on(&config, "agent_a", false).await;
    assert_followers_on(&config, "agent_b", true).await;
}

#[tokio::test]
async fn committed_rename_custom_workspace_is_not_residue() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = fixture(tmp.path());
    let custom = tmp.path().join("custom-ws");
    std::fs::create_dir_all(&custom).unwrap();
    std::fs::write(custom.join("keep"), b"stay").unwrap();
    config.agents.get_mut("agent_a").unwrap().workspace.path = Some(custom.clone());
    alias_refs::rename_with_cascade(&mut config, &AliasKind::Agent, "agent_a", "agent_b").unwrap();
    config.save().await.unwrap();

    let renamed = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let renamed_text = text_of(&renamed);
    assert!(!renamed.status.success(), "{renamed_text}");
    assert!(
        renamed_text.contains("is not configured"),
        "an alias-independent workspace is not stranded rename residue: {renamed_text}"
    );
    assert!(
        custom.join("keep").is_file(),
        "custom workspace was not moved"
    );
    assert!(custom.exists(), "custom workspace was not deleted");
}

#[cfg(feature = "gateway")]
#[tokio::test]
async fn committed_rename_gateway_recovery_is_visible_to_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let config = fixture(tmp.path());
    let old_ws = config.agent_workspace_dir("agent_a");
    std::fs::create_dir_all(&old_ws).unwrap();
    std::fs::write(old_ws.join("marker"), b"cross").unwrap();
    config.save().await.unwrap();
    let blocker = tmp.path().join("agents").join("agent_b");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"x").unwrap();

    let state = gateway_support::gateway_state(config);
    let response = zeroclaw::gateway::api_config::handle_rename_map_key(
        axum::extract::State(state),
        None,
        axum::Json(zeroclaw::gateway::api_config::RenameMapKeyBody {
            path: "agents".into(),
            from: "agent_a".into(),
            to: "agent_b".into(),
        }),
    )
    .await;
    assert!(
        response.status().is_success(),
        "gateway keeps the committed rename when the workspace move fails"
    );

    let blocked = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        !blocked.status.success(),
        "cli create sees recovery armed by the gateway: {}",
        text_of(&blocked)
    );

    std::fs::remove_file(&blocker).unwrap();
    std::fs::remove_dir_all(&old_ws).unwrap();
    let resumed = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    assert!(
        resumed.status.success(),
        "cli converges recovery the gateway armed: {}",
        text_of(&resumed)
    );
    let freed = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        freed.status.success(),
        "cli can reuse the alias after the shared recovery converges: {}",
        text_of(&freed)
    );
}

#[tokio::test]
async fn agents_delete_refuses_the_target_of_an_unfinished_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let (old_ws, blocker) = leave_rename_unfinished(tmp.path()).await;

    let target = run_agents(tmp.path(), &["delete", "agent_b", "--yes"]);
    let target_text = text_of(&target);
    assert!(
        !target.status.success(),
        "the unfinished rename still converges into agent_b: {target_text}"
    );
    assert!(
        target_text.contains("zeroclaw agents rename agent_a agent_b"),
        "the refusal names the rename to finish first: {target_text}"
    );
    assert!(
        configured_agents(tmp.path()).contains(&"agent_b".to_string()),
        "a refused delete leaves the config alone"
    );

    let retired = run_agents(tmp.path(), &["delete", "agent_a", "--yes"]);
    let retired_text = text_of(&retired);
    assert!(!retired.status.success(), "{retired_text}");
    assert!(
        retired_text.contains("zeroclaw agents rename agent_a agent_b"),
        "the retired alias names the rename that owns it: {retired_text}"
    );
    assert!(
        !retired_text.contains("is not configured"),
        "a retired alias is not reported as a missing entry: {retired_text}"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "a refused delete leaves the unmoved workspace in place"
    );

    std::fs::remove_file(&blocker).unwrap();
    let finished = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    assert!(
        finished.status.success(),
        "the rename resumes and converges: {}",
        text_of(&finished)
    );
    let deleted = run_agents(tmp.path(), &["delete", "agent_b", "--yes"]);
    assert!(
        deleted.status.success(),
        "a converged rename no longer guards its target: {}",
        text_of(&deleted)
    );
    assert!(!configured_agents(tmp.path()).contains(&"agent_b".to_string()));
}

#[tokio::test]
async fn agents_create_of_a_retired_alias_names_the_rename_to_rerun() {
    let tmp = tempfile::tempdir().unwrap();
    leave_rename_unfinished(tmp.path()).await;

    let created = run_agents(tmp.path(), &["create", "agent_a"]);
    let stderr = String::from_utf8_lossy(&created.stderr);
    assert!(
        !created.status.success(),
        "the retired alias cannot be created yet: {}",
        text_of(&created)
    );
    assert!(
        stderr.contains("zeroclaw agents rename agent_a agent_b"),
        "the refusal names the rename that frees the alias: {stderr}"
    );

    // The config surfaces that materialize an alias refuse it the same way.
    for args in [
        &[
            "config",
            "set",
            "--no-interactive",
            "agents.agent_a.enabled",
            "true",
        ][..],
        &["config", "init", "agents.agent_a"][..],
    ] {
        let refused = run_zeroclaw(tmp.path(), args);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success(),
            "`{}` must not recreate the retired alias: {}",
            args.join(" "),
            text_of(&refused)
        );
        assert!(
            stderr.contains("zeroclaw agents rename agent_a agent_b"),
            "`{}` names the rename that frees the alias: {stderr}",
            args.join(" ")
        );
    }
    assert!(
        !configured_agents(tmp.path()).contains(&"agent_a".to_string()),
        "a refused create writes nothing"
    );
}

/// Whether the recovery journal under `dir` holds the rename of `agent_a` to
/// `agent_b`.
fn rename_is_recorded(dir: &Path) -> bool {
    AgentRecoveryJournal::for_data_dir(&dir.join("data"))
        .load()
        .unwrap()
        .iter()
        .any(|record| record.from == "agent_a" && record.to == "agent_b")
}

#[tokio::test]
async fn agents_rename_abandon_drops_the_record_and_frees_the_old_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let (old_ws, _blocker) = leave_rename_unfinished(tmp.path()).await;
    assert!(
        rename_is_recorded(tmp.path()),
        "the unfinished rename leaves its recovery record"
    );

    let abandoned = run_agents(tmp.path(), &["rename", "agent_a", "agent_b", "--abandon"]);
    let abandoned_text = text_of(&abandoned);
    assert!(abandoned.status.success(), "{abandoned_text}");
    assert!(
        abandoned_text.contains("the old default workspace of `agent_a`"),
        "the state still kept under the old alias is listed: {abandoned_text}"
    );
    assert!(
        abandoned_text.contains("dropped the unfinished rename of agent_a to agent_b"),
        "{abandoned_text}"
    );
    assert!(!rename_is_recorded(tmp.path()), "the record is gone");
    assert!(old_ws.join("marker").is_file(), "abandoning moves nothing");

    let created = run_agents(tmp.path(), &["create", "agent_a"]);
    assert!(
        created.status.success(),
        "the old alias can be created again: {}",
        text_of(&created)
    );
    assert!(configured_agents(tmp.path()).contains(&"agent_a".to_string()));

    let again = run_agents(tmp.path(), &["rename", "agent_a", "agent_b", "--abandon"]);
    let again_text = text_of(&again);
    assert!(!again.status.success(), "{again_text}");
    assert!(
        again_text.contains("there is no unfinished rename of `agent_a` to `agent_b` to abandon"),
        "{again_text}"
    );
}

#[tokio::test]
async fn agents_rename_of_an_old_alias_configured_again_points_at_abandon() {
    let tmp = tempfile::tempdir().unwrap();
    let (old_ws, _blocker) = leave_rename_unfinished(tmp.path()).await;
    // A hand edit brings the retired alias back around the create guards.
    let config_path = tmp.path().join("config.toml");
    let mut saved = std::fs::read_to_string(&config_path).unwrap();
    saved.push_str("\n[agents.agent_a]\nrisk_profile = \"default\"\n");
    std::fs::write(&config_path, saved).unwrap();
    assert!(configured_agents(tmp.path()).contains(&"agent_a".to_string()));

    let refused = run_agents(tmp.path(), &["rename", "agent_a", "agent_b"]);
    let refused_text = text_of(&refused);
    assert!(!refused.status.success(), "{refused_text}");
    assert!(
        refused_text.contains("zeroclaw agents rename agent_a agent_b --abandon"),
        "the refusal names the way out: {refused_text}"
    );
    assert!(
        old_ws.join("marker").is_file(),
        "nothing moves while the old alias is configured again"
    );

    let abandoned = run_agents(tmp.path(), &["rename", "agent_a", "agent_b", "--abandon"]);
    assert!(
        abandoned.status.success(),
        "the record is dropped even while the old alias is configured again: {}",
        text_of(&abandoned)
    );
    assert!(!rename_is_recorded(tmp.path()));
}

#[tokio::test]
async fn agents_delete_previews_report_the_refusal_the_delete_makes() {
    let tmp = tempfile::tempdir().unwrap();
    let (old_ws, _blocker) = leave_rename_unfinished(tmp.path()).await;

    for args in [
        &["delete", "agent_b", "--dry-run"][..],
        &["delete", "agent_b"][..],
        &["delete", "agent_a", "--dry-run"][..],
    ] {
        let preview = run_agents(tmp.path(), args);
        let text = text_of(&preview);
        let command = args.join(" ");
        assert!(preview.status.success(), "`{command}` previews: {text}");
        assert!(
            text.contains("is BLOCKED"),
            "`{command}` reports the delete as blocked: {text}"
        );
        assert!(
            text.contains("zeroclaw agents rename agent_a agent_b"),
            "`{command}` names the rename to finish first: {text}"
        );
        assert!(
            !text.contains("would scrub"),
            "`{command}` does not preview an allowed delete: {text}"
        );
    }
    assert!(
        configured_agents(tmp.path()).contains(&"agent_b".to_string()),
        "a preview changes nothing"
    );
    assert!(old_ws.join("marker").is_file());
}

/// A gateway `AppState` over `config`, with no provider or store handles
/// beyond placeholders.
#[cfg(feature = "gateway")]
mod gateway_support {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::RwLock;
    use zeroclaw::gateway::{self, AppState};
    use zeroclaw_api::attribution::Attributable;
    use zeroclaw_config::schema::Config;
    use zeroclaw_memory::NoneMemory;
    use zeroclaw_providers::ModelProvider;
    use zeroclaw_runtime::security::PairingGuard;

    struct MockModelProvider;

    #[async_trait::async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    impl Attributable for MockModelProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    pub(super) fn gateway_state(config: Config) -> AppState {
        let memory: Arc<dyn zeroclaw_memory::Memory> =
            Arc::new(NoneMemory::new("agents-rename-recovery-cli-test"));
        AppState {
            config: Arc::new(RwLock::new(config)),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            agent_lifecycle: Default::default(),
            model_provider: Arc::new(MockModelProvider),
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    memory,
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            auto_save: false,
            pairing: Arc::new(PairingGuard::new(
                false,
                &[],
                zeroclaw_config::pairing::PairingCodePolicy::default(),
            )),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(gateway::GatewayRateLimiter::new(100, 100, 100)),
            auth_limiter: Arc::new(gateway::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(gateway::IdempotencyStore::new(
                Duration::from_secs(300),
                1000,
            )),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: HashMap::new(),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: HashMap::new(),
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            tools_registry_by_agent: Arc::new(HashMap::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(gateway::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(gateway::nodes::NodeRegistry::new(16)),
            mdns_peer_registry: gateway::nodes::mdns::MdnsPeerRegistry::default(),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: Arc::new(gateway::session_queue::SessionActorQueue::new(8, 30, 600)),
            device_registry: None,
            pending_pairings: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            #[cfg(feature = "webauthn")]
            webauthn: None,
            cancel_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
            sop_driver_handles: None,
        }
    }
}
