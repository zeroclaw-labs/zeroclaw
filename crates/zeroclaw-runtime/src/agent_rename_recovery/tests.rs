use super::*;
use crate::lifecycle_path::with_failing_sync;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use zeroclaw_api::memory_traits::{MemoryCategory, MemoryEntry};
use zeroclaw_config::agent_recovery_journal::{pending_target, retired_alias};
use zeroclaw_config::alias_refs::{self, AliasKind};
use zeroclaw_config::schema::AliasedAgentConfig;
use zeroclaw_memory::{NoneMemory, SqliteMemory};

const FROM: &str = "scout";
const TO: &str = "ranger";
/// A configured alias older than the alias grammar, and the valid alias
/// an operator migrates it to.
const LEGACY: &str = "Legacy-Agent";
const MIGRATED: &str = "legacy_agent";

/// An install under `tmp` whose only agents are `aliases`, each able to
/// own cron jobs. Nothing is created on disk.
fn fixture(tmp: &TempDir, aliases: &[&str]) -> Config {
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        data_dir: tmp.path().join("data"),
        ..Config::default()
    };
    config.agents.clear();
    config
        .risk_profiles
        .entry("default".to_string())
        .or_default();
    config
        .runtime_profiles
        .entry("default".to_string())
        .or_default();
    for alias in aliases {
        config.agents.insert((*alias).to_string(), agent());
    }
    config
}

fn agent() -> AliasedAgentConfig {
    AliasedAgentConfig {
        risk_profile: "default".into(),
        runtime_profile: "default".into(),
        ..AliasedAgentConfig::default()
    }
}

/// `config` with `from` renamed to `to` the way every surface commits it.
fn renamed(config: &Config, from: &str, to: &str) -> Config {
    let mut after = config.clone();
    alias_refs::rename_with_cascade(&mut after, &AliasKind::Agent, from, to).unwrap();
    after
}

fn committed(config: &Config) -> Config {
    renamed(config, FROM, TO)
}

/// A surface's commit: arm, commit, acknowledge. Nothing has converged.
async fn arm_and_commit(before: &Config) -> Config {
    let armed = arm(before, FROM, TO).await.unwrap();
    let after = committed(before);
    acknowledge_commit(&after, armed).await;
    after
}

/// Save the rename of `from` to `to` over the config file `before` was
/// saved to, the way a surface saves it: an incremental save of the
/// cascade's dirty paths. Returns the config the file now holds.
async fn save_renamed(before: &Config, from: &str, to: &str) -> Config {
    let mut after = before.clone();
    let report = alias_refs::rename_with_cascade(&mut after, &AliasKind::Agent, from, to).unwrap();
    for path in &report.dirty_paths {
        after.mark_dirty(path);
    }
    after.save_dirty().await.unwrap();
    after
}

fn records(config: &Config) -> Vec<RecoveryRecord> {
    AgentRecoveryJournal::for_config(config).load().unwrap()
}

fn outstanding(outcome: &ConvergeOutcome) -> &[FollowerIssue] {
    match outcome {
        ConvergeOutcome::Converged(_) => &[],
        ConvergeOutcome::Incomplete { outstanding, .. } => outstanding,
    }
}

fn lagging_followers(outcome: &ConvergeOutcome) -> Vec<FollowerKind> {
    outstanding(outcome)
        .iter()
        .map(|issue| issue.follower)
        .collect()
}

/// Put a file in `alias`'s alias-derived workspace, and return the
/// workspace.
fn seed_workspace(config: &Config, alias: &str) -> PathBuf {
    let workspace = config.default_agent_workspace_dir(alias);
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("MEMORY.md"), alias).unwrap();
    workspace
}

/// Put a file where `<install>/agents/<TO>` must be a directory.
fn block_destination(config: &Config) -> PathBuf {
    let alias_dir = config
        .default_agent_workspace_dir(TO)
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(alias_dir.parent().unwrap()).unwrap();
    std::fs::write(&alias_dir, "in the way").unwrap();
    alias_dir
}

fn sqlite_memory(config: &Config) -> SqliteMemory {
    SqliteMemory::new("sqlite", &config.data_dir).unwrap()
}

/// Give `alias` its memory identity and `rows` memories.
async fn seed_memory(config: &Config, alias: &str, rows: usize) {
    let memory = sqlite_memory(config);
    let agent_id = memory.ensure_agent_uuid(alias).await.unwrap();
    for idx in 0..rows {
        memory
            .store_with_agent(
                &format!("{alias}-{idx}"),
                "remembered",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&agent_id),
            )
            .await
            .unwrap();
    }
}

async fn memory_identity(config: &Config, alias: &str) -> usize {
    sqlite_memory(config).count_agent(alias).await.unwrap()
}

async fn memory_rows(config: &Config, alias: &str) -> usize {
    sqlite_memory(config)
        .export_agent(alias)
        .await
        .unwrap()
        .len()
}

/// Add a cron job owned by `alias`. Its risk-profile check creates the
/// owner's workspace, so that is pointed away from the one under test.
fn seed_cron(config: &Config, alias: &str) {
    let mut scratch = config.clone();
    if let Some(agent) = scratch.agents.get_mut(alias) {
        agent.workspace.path = Some(config.install_root_dir().join("scratch").join(alias));
    }
    crate::cron::add_job(&scratch, alias, "*/5 * * * *", "echo hello").unwrap();
}

fn cron_residue(config: &Config, alias: &str) -> Option<usize> {
    crate::cron::agent_residue_count(config, alias).unwrap()
}

fn seed_acp(config: &Config, session: &str, alias: &str, workspace: &Path) {
    AcpSessionStore::new(&config.data_dir)
        .unwrap()
        .create_session(session, alias, &workspace.to_string_lossy(), None)
        .unwrap();
}

/// The owner and working directory of an ACP session.
fn acp_row(config: &Config, session: &str) -> (String, PathBuf) {
    let row = AcpSessionStore::new(&config.data_dir)
        .unwrap()
        .load_session(session)
        .unwrap()
        .unwrap();
    (row.agent_alias, PathBuf::from(row.workspace_dir))
}

fn seed_session(config: &Config, session: &str, alias: &str) {
    SqliteSessionBackend::new(&config.data_dir)
        .unwrap()
        .set_session_agent_alias(session, alias)
        .unwrap();
}

fn session_owner(config: &Config, session: &str) -> Option<String> {
    SqliteSessionBackend::new(&config.data_dir)
        .unwrap()
        .get_session_agent_alias(session)
        .unwrap()
}

/// Seed every follower with state under `FROM`, and return the workspace.
async fn seed_every_follower(config: &Config) -> PathBuf {
    let workspace = seed_workspace(config, FROM);
    seed_memory(config, FROM, 2).await;
    seed_cron(config, FROM);
    seed_acp(config, "acp-1", FROM, &workspace);
    seed_session(config, "chat-1", FROM);
    workspace
}

/// A memory backend that keeps only per-agent identity counts and counts
/// the renames it is asked for. Like the SQL backends, it refuses to merge
/// into an alias that already owns memory.
#[derive(Default)]
struct CountingMemory {
    agents: parking_lot::Mutex<HashMap<String, usize>>,
    renames: AtomicUsize,
}

impl zeroclaw_api::attribution::Attributable for CountingMemory {
    fn role(&self) -> Role {
        Role::Memory(MemoryKind::Sqlite)
    }

    fn alias(&self) -> &str {
        "counting"
    }
}

#[async_trait::async_trait]
impl Memory for CountingMemory {
    fn name(&self) -> &str {
        "counting"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
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
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.agents.lock().values().sum())
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
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
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        self.renames.fetch_add(1, Ordering::SeqCst);
        let mut agents = self.agents.lock();
        if agents.get(to).is_some_and(|count| *count > 0) {
            anyhow::bail!("cannot rename agent memory to `{to}`: refusing to merge");
        }
        let moved = agents.remove(from).unwrap_or(0);
        if moved > 0 {
            agents.insert(to.to_string(), moved);
        }
        Ok(usize::from(moved > 0))
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        Ok(self.agents.lock().get(agent_alias).copied().unwrap_or(0))
    }
}

#[tokio::test]
async fn a_fresh_rename_moves_every_follower_and_clears_its_record() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_every_follower(&before).await;
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&before, FROM, TO, &stores).await.unwrap(),
        Disposition::Fresh
    );
    let armed = arm(&before, FROM, TO).await.unwrap();
    assert_eq!((armed.from(), armed.to()), (FROM, TO));
    assert_eq!(
        retired_alias(&before, FROM).unwrap(),
        None,
        "a prepared record retires nothing until its commit lands"
    );
    let after = committed(&before);
    acknowledge_commit(&after, armed).await;

    let recorded = records(&after);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].phase, RecoveryPhase::Committed);
    assert_eq!(
        recorded[0].source_workspace.as_deref(),
        Some(source.as_path())
    );
    assert!(retired_alias(&after, FROM).unwrap().is_some());
    assert!(pending_target(&after, TO).unwrap().is_some());

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(outcome.warnings().is_empty());
    let report = outcome.report();
    assert!(report.workspace_moved);
    assert_eq!(
        report.memory_rows, 1,
        "the identity row carries the memories"
    );
    assert_eq!(report.cron_jobs, 1);
    assert_eq!(report.acp_sessions, 1);
    assert_eq!(report.acp_workspaces, 1);
    assert_eq!(report.sessions_repointed, 1);

    let destination = after.default_agent_workspace_dir(TO);
    assert_eq!(
        std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
        FROM
    );
    assert!(
        !source.parent().unwrap().exists(),
        "the emptied old alias directory is removed"
    );
    assert_eq!(memory_identity(&after, FROM).await, 0);
    assert_eq!(memory_rows(&after, TO).await, 2);
    assert_eq!(cron_residue(&after, FROM), Some(0));
    assert_eq!(cron_residue(&after, TO), Some(1));
    assert_eq!(acp_row(&after, "acp-1"), (TO.to_string(), destination));
    assert_eq!(session_owner(&after, "chat-1").as_deref(), Some(TO));

    assert!(!AgentRecoveryJournal::for_config(&after).path().exists());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
    assert_eq!(pending_target(&after, TO).unwrap(), None);
}

#[tokio::test]
async fn a_rename_interrupted_after_its_commit_resumes() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    seed_cron(&before, FROM);
    let stores = SurfaceStores::none();

    let after = arm_and_commit(&before).await;
    // The process stops here: nothing has converged.
    assert!(source.exists());
    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(outcome.report().workspace_moved);
    assert_eq!(outcome.report().cron_jobs, 1);
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_rename_interrupted_before_acknowledging_its_commit_resumes() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_workspace(&before, FROM);
    let stores = SurfaceStores::none();

    let armed = arm(&before, FROM, TO).await.unwrap();
    let after = committed(&before);
    // The process stops after the commit, before acknowledging it.
    drop(armed);
    assert_eq!(records(&after)[0].phase, RecoveryPhase::Prepared);
    assert!(
        retired_alias(&after, FROM).unwrap().is_some(),
        "the live config shows the commit, so the prepared record retires the old alias"
    );

    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_rename_committed_without_a_record_is_discovered_and_recorded() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_cron(&before, FROM);
    // Committed with no record, as an older build did.
    let after = committed(&before);
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );
    let recorded = records(&after);
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        (
            recorded[0].from.as_str(),
            recorded[0].to.as_str(),
            recorded[0].phase
        ),
        (FROM, TO, RecoveryPhase::Committed)
    );
    assert_eq!(
        recorded[0].source_workspace,
        Some(after.default_agent_workspace_dir(FROM))
    );
    assert!(retired_alias(&after, FROM).unwrap().is_some());

    // Asking again resumes the same record rather than adding one.
    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );
    assert_eq!(records(&after).len(), 1);

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(cron_residue(&after, TO), Some(1));
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn discovery_that_cannot_read_a_store_still_retires_the_old_alias() {
    let tmp = TempDir::new().unwrap();
    let after = committed(&fixture(&tmp, &[FROM]));
    // A directory where the cron database belongs cannot be read.
    std::fs::create_dir_all(crate::cron::db_path(&after)).unwrap();

    let err = resolve(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, RenameRecoveryError::Unreadable { store, .. } if store == "cron"),
        "{err:?}"
    );
    assert!(
        err.to_string().starts_with("cron could not be read: "),
        "{err}"
    );
    let retired = retired_alias(&after, FROM)
        .unwrap()
        .expect("an unreadable follower may hold state, so the old alias stays retired");
    assert_eq!(retired.phase, RecoveryPhase::Committed);
    assert_eq!(retired.to, TO);
}

#[tokio::test]
async fn a_rename_with_nothing_left_behind_is_not_configured_and_records_nothing() {
    let tmp = TempDir::new().unwrap();
    let after = committed(&fixture(&tmp, &[FROM]));
    let stores = SurfaceStores::none();

    let err = resolve(&after, FROM, TO, &stores).await.unwrap_err();
    assert!(
        matches!(&err, RenameRecoveryError::NotConfigured { alias } if alias == FROM),
        "{err:?}"
    );
    assert_eq!(err.to_string(), format!("agents.{FROM} is not configured"));

    // Neither alias configured is not configured either.
    let neither = fixture(&tmp, &[]);
    assert!(matches!(
        resolve(&neither, FROM, TO, &stores).await,
        Err(RenameRecoveryError::NotConfigured { .. })
    ));
    assert!(
        !after.data_dir.exists(),
        "resolving created no journal, lock, or store"
    );
}

#[tokio::test]
async fn a_retried_rename_keeps_the_memory_identity_it_already_moved() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_workspace(&before, FROM);
    // An identity row with no memories under it.
    seed_memory(&before, FROM, 0).await;
    let after = arm_and_commit(&before).await;
    let blocker = block_destination(&after);
    let stores = SurfaceStores::none();

    let first = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
    assert_eq!(first.report().memory_rows, 1);
    assert_eq!(memory_identity(&after, TO).await, 1);

    std::fs::remove_file(&blocker).unwrap();
    let second = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(second.is_converged(), "{:?}", second.warnings());
    assert_eq!(
        second.report().memory_rows,
        0,
        "memory had nothing left to move"
    );
    assert_eq!(
        memory_identity(&after, TO).await,
        1,
        "a second rename would have dropped the moved identity as an orphan"
    );
    assert_eq!(memory_identity(&after, FROM).await, 0);
}

#[tokio::test]
async fn a_retried_rename_does_not_merge_memory_it_already_moved() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_workspace(&before, FROM);
    seed_memory(&before, FROM, 3).await;
    let after = arm_and_commit(&before).await;
    let blocker = block_destination(&after);
    let stores = SurfaceStores::none();

    let first = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
    assert_eq!(memory_rows(&after, TO).await, 3);

    std::fs::remove_file(&blocker).unwrap();
    let second = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(second.is_converged(), "{:?}", second.warnings());
    assert_eq!(memory_rows(&after, TO).await, 3);
    assert_eq!(memory_rows(&after, FROM).await, 0);
}

#[tokio::test]
async fn a_genuine_memory_handle_is_renamed_through_exactly_once() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    let blocker = block_destination(&after);
    let counting = Arc::new(CountingMemory::default());
    counting.agents.lock().insert(FROM.to_string(), 1);
    let memory: Arc<dyn Memory> = counting.clone();
    let stores = SurfaceStores {
        memory: Some(&memory),
        ..SurfaceStores::none()
    };

    let first = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
    assert_eq!(first.report().memory_rows, 1);

    std::fs::remove_file(&blocker).unwrap();
    let second = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(second.is_converged(), "{:?}", second.warnings());
    assert_eq!(
        counting.renames.load(Ordering::SeqCst),
        1,
        "a follower with nothing under the old alias is not renamed again"
    );
    assert_eq!(counting.agents.lock().get(TO).copied(), Some(1));
}

#[tokio::test]
async fn a_none_memory_placeholder_does_not_hide_the_configured_store() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_memory(&before, FROM, 2).await;
    let after = arm_and_commit(&before).await;
    // What a surface holds when its configured backend failed to build.
    let placeholder: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
    let stores = SurfaceStores {
        memory: Some(&placeholder),
        ..SurfaceStores::none()
    };

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().memory_rows, 1);
    assert_eq!(memory_rows(&after, TO).await, 2);
}

#[tokio::test]
async fn run_history_left_under_the_old_alias_is_found_and_moved() {
    let tmp = TempDir::new().unwrap();
    let after = committed(&fixture(&tmp, &[FROM]));
    // A finished one-shot: its job row is gone, and its run row is still
    // cleanup-owned by the old alias.
    let now = chrono::Utc::now();
    crate::cron::record_run(
        &after,
        "finished-one-shot",
        now,
        now,
        "ok",
        crate::cron::RunOutcomes {
            execution: "ok",
            delivery: "not_required",
            persistence: "not_bound",
        },
        crate::cron::RunProvenance {
            principal: None,
            executing_agent: Some(FROM),
            job_source: Some("imperative"),
        },
        Some("done"),
        1,
    )
    .unwrap();
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().cron_jobs, 0, "only run history moved");
    assert_eq!(cron_residue(&after, FROM), Some(0));
    assert_eq!(cron_residue(&after, TO), Some(1));
}

#[tokio::test]
async fn invalid_or_reserved_aliases_are_refused_before_touching_disk() {
    let tmp = TempDir::new().unwrap();
    let config = fixture(&tmp, &[FROM]);
    let stores = SurfaceStores::none();

    for (from, to) in [
        ("../x", TO),
        (FROM, "../x"),
        ("default", TO),
        (FROM, "default"),
        (FROM, FROM),
    ] {
        let refusals = [
            resolve(&config, from, to, &stores).await.unwrap_err(),
            arm(&config, from, to).await.unwrap_err(),
            converge(&config, from, to, &stores).await.unwrap_err(),
        ];
        for refusal in refusals {
            let expected = if from == "default" || to == "default" {
                matches!(refusal, RenameRecoveryError::ReservedAlias { .. })
            } else {
                matches!(refusal, RenameRecoveryError::InvalidAlias { .. })
            };
            assert!(expected, "{from} -> {to}: {refusal:?}");
        }
    }
    assert!(!config.data_dir.exists(), "no journal or lock was created");
    let agents_dir = config
        .default_agent_workspace_dir(FROM)
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    assert!(
        !agents_dir.exists(),
        "nothing under the agents directory was created"
    );
}

#[tokio::test]
async fn a_configured_legacy_alias_renames_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[LEGACY]);
    assert!(
        zeroclaw_config::helpers::validate_alias_key(LEGACY).is_err(),
        "the fixture alias must predate the alias grammar"
    );
    let source = seed_workspace(&before, LEGACY);
    seed_memory(&before, LEGACY, 1).await;
    seed_cron(&before, LEGACY);
    seed_acp(&before, "acp-legacy", LEGACY, &source);
    seed_session(&before, "chat-legacy", LEGACY);
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&before, LEGACY, MIGRATED, &stores).await.unwrap(),
        Disposition::Fresh
    );
    let armed = arm(&before, LEGACY, MIGRATED).await.unwrap();
    let after = renamed(&before, LEGACY, MIGRATED);
    acknowledge_commit(&after, armed).await;
    assert!(retired_alias(&after, LEGACY).unwrap().is_some());

    let outcome = converge(&after, LEGACY, MIGRATED, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    let report = outcome.report();
    assert!(report.workspace_moved);
    assert_eq!(
        (
            report.memory_rows,
            report.cron_jobs,
            report.acp_sessions,
            report.acp_workspaces,
            report.sessions_repointed,
        ),
        (1, 1, 1, 1, 1)
    );
    let destination = after.default_agent_workspace_dir(MIGRATED);
    assert_eq!(
        std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
        LEGACY
    );
    assert!(!source.exists());
    assert_eq!(memory_rows(&after, MIGRATED).await, 1);
    assert_eq!(cron_residue(&after, MIGRATED), Some(1));
    assert_eq!(
        acp_row(&after, "acp-legacy"),
        (MIGRATED.to_string(), destination)
    );
    assert_eq!(
        session_owner(&after, "chat-legacy").as_deref(),
        Some(MIGRATED)
    );
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_recorded_rename_of_a_legacy_alias_resumes_after_a_crash() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[LEGACY]);
    let source = seed_workspace(&before, LEGACY);
    seed_cron(&before, LEGACY);
    let armed = arm(&before, LEGACY, MIGRATED).await.unwrap();
    let after = renamed(&before, LEGACY, MIGRATED);
    acknowledge_commit(&after, armed).await;
    // The process stops here. The old alias is no longer configured, so
    // only the journal record still names it.
    assert!(after.agent(LEGACY).is_none());
    assert!(source.exists());
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&after, LEGACY, MIGRATED, &stores).await.unwrap(),
        Disposition::Resume
    );
    let outcome = converge(&after, LEGACY, MIGRATED, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(outcome.report().workspace_moved);
    assert_eq!(outcome.report().cron_jobs, 1);
    assert!(!source.exists());
    assert!(records(&after).is_empty());

    // Once the record is cleared nothing names the old alias, so a request
    // naming it again is held to the alias grammar.
    assert!(matches!(
        resolve(&after, LEGACY, MIGRATED, &stores).await,
        Err(RenameRecoveryError::InvalidAlias { alias, .. }) if alias == LEGACY
    ));
}

#[tokio::test]
async fn a_custom_workspace_never_moves() {
    let tmp = TempDir::new().unwrap();
    let mut before = fixture(&tmp, &[FROM]);
    let custom = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&custom).unwrap();
    std::fs::write(custom.join("MEMORY.md"), "custom").unwrap();
    before.agents.get_mut(FROM).unwrap().workspace.path = Some(custom.clone());
    let after = committed(&before);
    assert_eq!(after.agent_workspace_dir(TO), custom);
    let stores = SurfaceStores::none();

    // Nothing is at the old default location, so a rename committed
    // without a record left nothing behind.
    let err = resolve(&after, FROM, TO, &stores).await.unwrap_err();
    assert!(
        matches!(err, RenameRecoveryError::NotConfigured { .. }),
        "{err:?}"
    );
    assert!(!after.data_dir.exists());

    // A directory at the old default location is not this agent's
    // workspace, but an agent re-created under the old alias would adopt
    // it, so a recorded rename does not finish while it holds anything.
    let leftover = seed_workspace(&before, FROM);
    let armed = arm(&before, FROM, TO).await.unwrap();
    acknowledge_commit(&after, armed).await;
    assert_eq!(records(&after)[0].source_workspace, None);
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
            leftover.display()
        )
    );
    assert!(!outcome.report().workspace_moved);
    assert!(leftover.join("MEMORY.md").is_file(), "nothing is deleted");
    assert!(retired_alias(&after, FROM).unwrap().is_some());

    // Emptied by hand, it is removed and the rename finishes; neither
    // workspace ever moved.
    std::fs::remove_file(leftover.join("MEMORY.md")).unwrap();
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(!outcome.report().workspace_moved);
    assert!(!leftover.parent().unwrap().exists());
    assert_eq!(
        std::fs::read_to_string(custom.join("MEMORY.md")).unwrap(),
        "custom"
    );
    assert!(!after.default_agent_workspace_dir(TO).exists());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn an_old_alias_pinned_to_its_default_workspace_never_shares_it() {
    let tmp = TempDir::new().unwrap();
    let mut before = fixture(&tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    // `from` names its own default location as an explicit path, which
    // the rename hands to `to` unchanged.
    before.agents.get_mut(FROM).unwrap().workspace.path = Some(old.clone());
    let mut after = arm_and_commit(&before).await;
    assert_eq!(
        records(&after)[0].source_workspace,
        None,
        "an explicit path is recorded as custom"
    );
    assert_eq!(after.agent_workspace_dir(TO), old);
    let stores = SurfaceStores::none();

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the workspace.path of agent `{TO}` is or overlaps the old default workspace of `{FROM}`; point workspace.path elsewhere, then re-run the rename"
        )
    );
    assert!(old.join("MEMORY.md").is_file(), "nothing moved or deleted");
    assert!(retired_alias(&after, FROM).unwrap().is_some());
    // The same path spelled another way is the same directory.
    after.agents.get_mut(TO).unwrap().workspace.path = Some(old.join("..").join("workspace"));
    let spelled = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(outstanding(&spelled)[0].kind, FollowerIssueKind::Conflict);

    // Pointed elsewhere, the old directory still holds files.
    let elsewhere = tmp.path().join("elsewhere");
    after.agents.get_mut(TO).unwrap().workspace.path = Some(elsewhere.clone());
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
            old.display()
        )
    );

    // Once its files are moved by hand the empty directory is removed.
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(old.join("MEMORY.md"), elsewhere.join("MEMORY.md")).unwrap();
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(!outcome.report().workspace_moved);
    assert!(!old.parent().unwrap().exists());
    assert!(elsewhere.join("MEMORY.md").is_file());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_custom_path_set_after_a_conflict_does_not_strand_the_old_workspace() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    let mut after = arm_and_commit(&before).await;
    let destination = after.default_agent_workspace_dir(TO);
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.md"), "theirs").unwrap();
    let stores = SurfaceStores::none();
    let first = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(outstanding(&first)[0].kind, FollowerIssueKind::Conflict);

    // Rather than clearing the destination, the operator gives the new
    // alias a custom workspace and re-runs the rename.
    let custom = tmp.path().join("custom");
    after.agents.get_mut(TO).unwrap().workspace.path = Some(custom.clone());
    let second = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&second);
    assert_eq!(issues.len(), 1, "{:?}", second.warnings());
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
            old.display()
        )
    );
    assert!(old.join("MEMORY.md").is_file());
    let mut recreated = after.clone();
    assert!(
        matches!(
            alias_refs::create_map_key_checked(&mut recreated, "agents", FROM),
            Err(alias_refs::CreateError::Retired { .. })
        ),
        "the old alias stays retired while its old workspace is there"
    );

    std::fs::create_dir_all(&custom).unwrap();
    std::fs::rename(old.join("MEMORY.md"), custom.join("MEMORY.md")).unwrap();
    let third = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(third.is_converged(), "{:?}", third.warnings());
    assert!(!old.exists());
    assert_eq!(
        std::fs::read_to_string(destination.join("keep.md")).unwrap(),
        "theirs"
    );
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_custom_workspace_overlapping_the_old_default_keeps_the_record() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let mut after = arm_and_commit(&before).await;
    let old = after.default_agent_workspace_dir(FROM);
    let old_alias_dir = old.parent().unwrap().to_path_buf();
    let stores = SurfaceStores::none();
    let overlapping = format!(
        "the workspace.path of agent `{TO}` is or overlaps the old default workspace of `{FROM}`; point workspace.path elsewhere, then re-run the rename"
    );
    let assert_overlap = |outcome: &ConvergeOutcome, custom: &Path| {
        let issues = outstanding(outcome);
        assert_eq!(
            issues.len(),
            1,
            "{}: {:?}",
            custom.display(),
            outcome.warnings()
        );
        assert_eq!(issues[0].follower, FollowerKind::Workspace);
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(issues[0].to_string(), overlapping, "{}", custom.display());
        assert!(!outcome.report().workspace_moved);
        // `before` reads the same journal as `after`, which changes below.
        assert_eq!(records(&before).len(), 1, "{}", custom.display());
    };

    // Beneath the old default workspace, or containing it as the old
    // alias directory or the install itself does, whether or not either
    // exists yet.
    let beneath = old.join("proj");
    assert!(!old.exists());
    for custom in [
        beneath.clone(),
        old_alias_dir.clone(),
        tmp.path().to_path_buf(),
    ] {
        let mut config = after.clone();
        config.agents.get_mut(TO).unwrap().workspace.path = Some(custom.clone());
        let outcome = converge(&config, FROM, TO, &stores).await.unwrap();
        assert_overlap(&outcome, &custom);
    }
    assert!(!old_alias_dir.exists(), "nothing was created");

    // With files in the old default workspace the overlap is still what
    // is reported, not the leftover.
    seed_workspace(&before, FROM);
    after.agents.get_mut(TO).unwrap().workspace.path = Some(beneath.clone());
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_overlap(&outcome, &beneath);
    assert!(!beneath.exists());

    // Spelled through a symlink, the same directories still overlap.
    #[cfg(unix)]
    {
        std::fs::create_dir(&beneath).unwrap();
        let links = [
            (tmp.path().join("alias-link"), old_alias_dir.clone()),
            (tmp.path().join("old-link"), old.clone()),
            (tmp.path().join("proj-link"), beneath.clone()),
        ];
        for (link, target) in links {
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let mut config = after.clone();
            config.agents.get_mut(TO).unwrap().workspace.path = Some(link.clone());
            let outcome = converge(&config, FROM, TO, &stores).await.unwrap();
            assert_overlap(&outcome, &link);
        }
        std::fs::remove_dir(&beneath).unwrap();
    }

    // A sibling whose name merely starts the same way does not overlap:
    // the old directory is reported as a leftover instead.
    let sibling = old_alias_dir.join("workspace2");
    std::fs::create_dir(&sibling).unwrap();
    after.agents.get_mut(TO).unwrap().workspace.path = Some(sibling);
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
            old.display()
        )
    );
    assert_eq!(records(&after).len(), 1);

    // Pointed elsewhere, with the old files moved by hand, the rename
    // finishes.
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(old.join("MEMORY.md"), elsewhere.join("MEMORY.md")).unwrap();
    after.agents.get_mut(TO).unwrap().workspace.path = Some(elsewhere.clone());
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(!old.exists());
    assert!(elsewhere.join("MEMORY.md").is_file());
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

/// Create the install directory `<tmp>/install` and a symlink to it,
/// `<tmp>/install-link`, and return the default workspace of `FROM` spelled
/// through each. Nothing beneath the install exists.
#[cfg(unix)]
fn workspace_through_a_link(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let install = tmp.path().join("install");
    std::fs::create_dir(&install).unwrap();
    let link = tmp.path().join("install-link");
    std::os::unix::fs::symlink(&install, &link).unwrap();
    let workspace = |root: &Path| root.join("agents").join(FROM).join("workspace");
    (workspace(&install), workspace(&link))
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_workspace_beneath_the_old_default_through_a_symlink_keeps_the_record() {
    let tmp = TempDir::new().unwrap();
    let (old, linked_old) = workspace_through_a_link(&tmp);
    let mut before = fixture(&tmp, &[FROM]);
    before.config_path = tmp.path().join("install").join("config.toml");
    assert_eq!(before.default_agent_workspace_dir(FROM), old);
    // The old alias has a custom workspace that does not exist yet, spelled
    // through the link, beneath where its default workspace would be. The
    // rename hands it to the new alias unchanged. Neither path exists, so
    // only resolving the link shows that they overlap.
    let custom = linked_old.join("project");
    before.agents.get_mut(FROM).unwrap().workspace.path = Some(custom.clone());
    let after = arm_and_commit(&before).await;
    assert_eq!(after.agent_workspace_dir(TO), custom);
    assert!(!old.exists());
    assert!(!custom.exists());

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].follower, FollowerKind::Workspace);
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the workspace.path of agent `{TO}` is or overlaps the old default workspace of `{FROM}`; point workspace.path elsewhere, then re-run the rename"
        )
    );
    assert!(!outcome.report().workspace_moved);
    assert_eq!(records(&after).len(), 1);
    assert!(retired_alias(&after, FROM).unwrap().is_some());
    let mut recreated = after.clone();
    assert!(
        matches!(
            alias_refs::create_map_key_checked(&mut recreated, "agents", FROM),
            Err(alias_refs::CreateError::Retired { .. })
        ),
        "the old alias stays retired while the new workspace lies in its default one"
    );
    assert!(!old.exists(), "nothing was created");

    // Once the new alias creates its workspace, it lies inside the one an
    // agent re-created under the old alias would take as its own.
    std::fs::create_dir_all(&custom).unwrap();
    assert!(
        std::fs::canonicalize(&custom)
            .unwrap()
            .starts_with(std::fs::canonicalize(&old).unwrap())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_ancestor_cannot_hide_an_overlap_between_missing_paths() {
    let tmp = TempDir::new().unwrap();
    let (old, linked_old) = workspace_through_a_link(&tmp);
    let parent = |path: &Path| path.parent().unwrap().to_path_buf();
    // At the old default workspace, beneath it, or containing it, with the
    // link on either side and in either argument.
    let pairs = [
        (linked_old.clone(), old.clone()),
        (linked_old.join("project"), old.clone()),
        (old.join("project"), linked_old.clone()),
        (parent(&linked_old), old.clone()),
        (parent(&old), linked_old.clone()),
    ];
    for (a, b) in &pairs {
        for (a, b) in [(a, b), (b, a)] {
            assert!(
                workspaces_overlap(a, b).await.unwrap(),
                "{} and {}",
                a.display(),
                b.display()
            );
        }
    }
    assert!(!parent(&parent(&old)).exists(), "nothing was created");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_ancestor_keeps_disjoint_missing_paths_apart() {
    let tmp = TempDir::new().unwrap();
    let (old, linked_old) = workspace_through_a_link(&tmp);
    let linked_agents = linked_old.parent().unwrap().parent().unwrap();
    // Another alias's workspace, and a sibling whose name merely starts
    // like the old default workspace.
    for custom in [
        linked_agents.join(TO).join("workspace"),
        linked_agents.join(FROM).join("workspace2"),
    ] {
        for (a, b) in [(&custom, &old), (&old, &custom)] {
            assert!(
                !workspaces_overlap(a, b).await.unwrap(),
                "{} and {}",
                a.display(),
                b.display()
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_overlap_past_dotdot_in_the_missing_part_cannot_be_ruled_out() {
    let tmp = TempDir::new().unwrap();
    let old = tmp.path().join("agents").join(FROM).join("workspace");
    // Folded, this is `<tmp>/project`, but on Unix `..` steps out of
    // whatever `link` turns out to be once it is created.
    let custom = tmp.path().join("link").join("..").join("project");
    for (a, b) in [(&custom, &old), (&old, &custom)] {
        let result = workspaces_overlap(a, b).await;
        assert!(result.is_err(), "{result:?}");
    }

    // Created as a symlink into the old default workspace, it leads there.
    std::fs::create_dir_all(old.join("inner")).unwrap();
    std::os::unix::fs::symlink(old.join("inner"), tmp.path().join("link")).unwrap();
    assert!(workspaces_overlap(&custom, &old).await.unwrap());
}

#[cfg(unix)]
#[tokio::test]
async fn an_overlap_through_a_dangling_symlink_cannot_be_ruled_out() {
    let tmp = TempDir::new().unwrap();
    let old = tmp.path().join("agents").join(FROM).join("workspace");
    // A link to the old default workspace, which does not exist yet.
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&old, &link).unwrap();
    for custom in [link.clone(), link.join("project")] {
        for (a, b) in [(&custom, &old), (&old, &custom)] {
            let result = workspaces_overlap(a, b).await;
            assert!(result.is_err(), "{}: {result:?}", custom.display());
        }
    }

    // Once its target exists, the link leads into it.
    std::fs::create_dir_all(&old).unwrap();
    assert!(
        workspaces_overlap(&link.join("project"), &old)
            .await
            .unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_overlap_through_an_unreadable_directory_cannot_be_ruled_out() {
    use std::os::unix::fs::PermissionsExt;

    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root can search any directory");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let old = tmp.path().join("agents").join(FROM).join("workspace");
    let locked = tmp.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    let custom = locked.join("project");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let overlap = workspaces_overlap(&custom, &old).await;
    let resolved = resolved_location(&custom);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        overlap.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        resolved.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[tokio::test]
async fn paths_differing_only_in_case_overlap_where_the_filesystem_ignores_case() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("Install")).unwrap();
    let old = tmp
        .path()
        .join("Install")
        .join("agents")
        .join(FROM)
        .join("workspace");
    // Where case is ignored, the existing `INSTALL` resolves to `Install`
    // and the missing rest keeps the case it is given.
    let custom = tmp
        .path()
        .join("INSTALL")
        .join("AGENTS")
        .join("Scout")
        .join("workspace")
        .join("project");
    let overlap = workspaces_overlap(&custom, &old).await.unwrap();
    #[cfg(any(target_os = "macos", windows))]
    assert!(overlap, "{} lies in {}", custom.display(), old.display());
    #[cfg(not(any(target_os = "macos", windows)))]
    assert!(
        !overlap,
        "{} is apart from {}",
        custom.display(),
        old.display()
    );
}

#[tokio::test]
async fn a_recorded_workspace_outside_this_install_is_never_moved() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    // The journal names a workspace that is not the old alias's default
    // location under this config, as after the install moved.
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("MEMORY.md"), "elsewhere").unwrap();
    let journal = AgentRecoveryJournal::for_config(&after);
    let mut record = records(&after).remove(0);
    record.source_workspace = Some(elsewhere.clone());
    let guard = journal.lock(Duration::ZERO).unwrap();
    journal.upsert(&guard, record).unwrap();
    drop(guard);

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "the recorded workspace {} is not the default workspace of `{FROM}` under this config",
            elsewhere.display()
        )
    );
    assert!(!outcome.report().workspace_moved);
    assert!(elsewhere.join("MEMORY.md").is_file());
    assert!(old.join("MEMORY.md").is_file());
    assert!(!after.default_agent_workspace_dir(TO).exists());
    assert_eq!(records(&after).len(), 1);
}

#[tokio::test]
async fn a_case_only_rename_converges_on_either_kind_of_filesystem() {
    const UPPER: &str = "Scout";
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[UPPER]);
    let source = seed_workspace(&before, UPPER);
    seed_acp(&before, "acp-case", UPPER, &source);
    let destination = before.default_agent_workspace_dir(FROM);
    // Whether the host folds case decides what converging means.
    let one_directory = same_existing_file(&source, &destination).unwrap();

    let armed = arm(&before, UPPER, FROM).await.unwrap();
    let after = renamed(&before, UPPER, FROM);
    acknowledge_commit(&after, armed).await;
    let outcome = converge(&after, UPPER, FROM, &SurfaceStores::none())
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(
        std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
        UPPER
    );
    if one_directory {
        assert!(
            !outcome.report().workspace_moved,
            "one directory under both names is already in place"
        );
        assert!(source.join("MEMORY.md").is_file());
    } else {
        assert!(outcome.report().workspace_moved);
        assert!(!source.parent().unwrap().exists());
    }
    assert_eq!(acp_row(&after, "acp-case"), (FROM.to_string(), destination));
    assert!(records(&after).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_workspace_shared_through_a_symlink_is_not_converged() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    // The new alias's directory is a link to the old one, so both default
    // workspaces are one directory without being a case-only rename.
    std::os::unix::fs::symlink(
        old.parent().unwrap(),
        after.default_agent_workspace_dir(TO).parent().unwrap(),
    )
    .unwrap();
    assert!(same_existing_file(&old, &after.default_agent_workspace_dir(TO)).unwrap());

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert_eq!(
        lagging_followers(&outcome),
        vec![FollowerKind::Workspace],
        "an agent re-created under the old alias would share it"
    );
    assert!(old.join("MEMORY.md").is_file());
    assert_eq!(records(&after).len(), 1);
}

#[tokio::test]
async fn a_rename_whose_old_alias_is_configured_again_moves_nothing() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    seed_memory(&before, FROM, 2).await;
    let after = arm_and_commit(&before).await;
    // A hand edit brings the old alias back around the create guards.
    let mut readded = after.clone();
    readded.agents.insert(FROM.to_string(), agent());
    let stores = SurfaceStores::none();

    let refusals = [
        resolve(&readded, FROM, TO, &stores).await.unwrap_err(),
        arm(&readded, FROM, TO).await.unwrap_err(),
        converge(&readded, FROM, TO, &stores).await.unwrap_err(),
        resolve(&readded, FROM, "wolf", &stores).await.unwrap_err(),
        converge(&readded, FROM, "wolf", &stores).await.unwrap_err(),
    ];
    for refusal in &refusals {
        assert!(
            matches!(refusal, RenameRecoveryError::SourceReconfigured { from, to } if from == FROM && to == TO),
            "{refusal:?}"
        );
    }
    assert_eq!(
        refusals[0].to_string(),
        "agent `scout` is configured again while an earlier rename of it is unfinished; remove `[agents.scout]` from the config by hand, or abandon that rename, then retry"
    );
    assert!(
        !refusals[3].to_string().contains(TO),
        "a request naming another target must not learn the pending one: {}",
        refusals[3]
    );
    assert!(source.join("MEMORY.md").is_file(), "nothing moved");
    assert!(!after.default_agent_workspace_dir(TO).exists());
    assert_eq!(memory_identity(&after, FROM).await, 1);
    let recorded = records(&after);
    assert_eq!(recorded.len(), 1, "the record is untouched");
    assert_eq!(recorded[0].phase, RecoveryPhase::Committed);

    // With the hand-added entry removed again, the same rename finishes.
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(memory_rows(&after, TO).await, 2);
}

#[tokio::test]
async fn a_prepared_record_retires_the_old_alias_and_converge_commits_it() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_workspace(&before, FROM);
    let armed = arm(&before, FROM, TO).await.unwrap();
    let after = committed(&before);
    // The process stops before acknowledging the commit, and another door
    // then removes the target.
    drop(armed);
    let mut without_target = after.clone();
    without_target.agents.remove(TO);
    assert_eq!(records(&after)[0].phase, RecoveryPhase::Prepared);
    assert!(
        retired_alias(&without_target, FROM).unwrap().is_some(),
        "the old alias is gone, so it stays retired whatever became of the target"
    );
    assert!(matches!(
        ensure_alias_not_retired(&without_target, FROM).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));

    // Converging commits the record before anything moves.
    let blocker = block_destination(&after);
    let stores = SurfaceStores::none();
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(!outcome.is_converged());
    assert_eq!(records(&after)[0].phase, RecoveryPhase::Committed);
    assert!(
        retired_alias(&before, FROM).unwrap().is_some(),
        "a committed record keeps the old alias retired even if it comes back"
    );

    std::fs::remove_file(&blocker).unwrap();
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn an_unacknowledged_commit_keeps_the_old_alias_retired_when_it_comes_back() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let armed = arm(&before, FROM, TO).await.unwrap();
    let after = committed(&before);
    // The process stops after the commit, before acknowledging it, and a
    // hand edit then brings the old alias back beside the new one.
    drop(armed);
    let mut readded = after.clone();
    readded.agents.insert(FROM.to_string(), agent());
    let stores = SurfaceStores::none();

    let retired = retired_alias(&readded, FROM)
        .unwrap()
        .expect("both aliases configured is ambiguous, so the old alias stays retired");
    assert_eq!(
        (retired.to.as_str(), retired.phase),
        (TO, RecoveryPhase::Prepared)
    );
    assert!(pending_target(&readded, TO).unwrap().is_some());
    assert!(matches!(
        ensure_alias_not_retired(&readded, FROM).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));
    let mut recreate = readded.clone();
    assert!(matches!(
        alias_refs::create_map_key_checked(&mut recreate, "agents", FROM),
        Err(alias_refs::CreateError::Retired { .. })
    ));

    // The contract treats it as a committed rename whose old alias is
    // configured again, and another rename of the old alias must not
    // collect the record as void.
    let refusals = [
        resolve(&readded, FROM, TO, &stores).await.unwrap_err(),
        arm(&readded, FROM, "wolf").await.unwrap_err(),
        converge(&readded, FROM, TO, &stores).await.unwrap_err(),
    ];
    for refusal in &refusals {
        assert!(
            matches!(refusal, RenameRecoveryError::SourceReconfigured { from, to } if from == FROM && to == TO),
            "{refusal:?}"
        );
    }
    let recorded = records(&readded);
    assert_eq!(recorded.len(), 1, "the record survives: {recorded:?}");
    assert_eq!(
        (
            recorded[0].from.as_str(),
            recorded[0].to.as_str(),
            recorded[0].phase
        ),
        (FROM, TO, RecoveryPhase::Prepared)
    );
    assert!(source.join("MEMORY.md").is_file(), "nothing moved");

    // With the hand-added entry gone again, the same rename finishes.
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn a_target_created_beside_an_uncommitted_rename_keeps_it_open_until_abandoned() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let armed = arm(&before, FROM, TO).await.unwrap();
    // The process stops before the commit, and another door then creates
    // the new alias while the old one is still configured.
    drop(armed);
    assert_eq!(retired_alias(&before, FROM).unwrap(), None);
    let mut both = before.clone();
    both.agents.insert(TO.to_string(), agent());

    let record = records(&both).remove(0);
    assert_eq!(record.phase, RecoveryPhase::Prepared);
    assert!(
        record.is_effective(&both),
        "both aliases configured is ambiguous, so the record fails closed"
    );
    assert_eq!(pending_target(&both, TO).unwrap(), Some(record.clone()));
    assert!(matches!(
        ensure_not_pending_target(&both, TO).await,
        Err(RenameRecoveryError::RecoveryPending { .. })
    ));
    assert!(matches!(
        ensure_alias_not_retired(&both, FROM).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));
    assert!(matches!(
        resolve(&both, FROM, TO, &SurfaceStores::none()).await,
        Err(RenameRecoveryError::SourceReconfigured { .. })
    ));

    // Abandoning the rename is the way out, and it moves nothing.
    let abandoned = abandon_rename(&both, FROM, TO).await.unwrap();
    assert_eq!(abandoned.record, record);
    assert!(records(&both).is_empty());
    assert!(source.join("MEMORY.md").is_file());
    assert_eq!(pending_target(&both, TO).unwrap(), None);
    assert_eq!(retired_alias(&both, FROM).unwrap(), None);
    assert!(ensure_not_pending_target(&both, TO).await.is_ok());
}

#[tokio::test]
async fn arming_a_rename_onto_a_configured_alias_is_refused_and_records_nothing() {
    let tmp = TempDir::new().unwrap();
    let both = fixture(&tmp, &[FROM, TO]);

    let err = arm(&both, FROM, TO).await.unwrap_err();
    assert!(
        matches!(&err, RenameRecoveryError::InvalidAlias { alias, .. } if alias == TO),
        "{err:?}"
    );
    assert!(
        records(&both).is_empty(),
        "a rename that cannot commit is never recorded"
    );
    assert_eq!(retired_alias(&both, FROM).unwrap(), None);
    assert_eq!(pending_target(&both, TO).unwrap(), None);
}

#[tokio::test]
async fn a_fresh_rename_is_void_until_its_commit_lands() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let stores = SurfaceStores::none();

    assert_eq!(
        resolve(&before, FROM, TO, &stores).await.unwrap(),
        Disposition::Fresh
    );
    let armed = arm(&before, FROM, TO).await.unwrap();
    // Between the arm and the commit the old alias is configured and the
    // new one is not, which proves the commit has not landed.
    let prepared = records(&before).remove(0);
    assert_eq!(prepared.phase, RecoveryPhase::Prepared);
    assert!(!prepared.is_effective(&before));
    assert_eq!(retired_alias(&before, FROM).unwrap(), None);
    assert_eq!(pending_target(&before, TO).unwrap(), None);
    assert!(ensure_alias_not_retired(&before, FROM).await.is_ok());
    assert!(ensure_not_pending_target(&before, TO).await.is_ok());

    let after = committed(&before);
    assert!(prepared.is_effective(&after), "the commit landed");
    acknowledge_commit(&after, armed).await;
    assert_eq!(records(&after)[0].phase, RecoveryPhase::Committed);

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(outcome.report().workspace_moved);
    assert!(!source.exists());
    assert!(
        after
            .default_agent_workspace_dir(TO)
            .join("MEMORY.md")
            .is_file()
    );
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

#[tokio::test]
async fn a_memory_merge_refusal_is_a_conflict() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_memory(&before, FROM, 2).await;
    // The new alias already owns memory of its own.
    seed_memory(&before, TO, 1).await;
    let after = arm_and_commit(&before).await;

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].follower, FollowerKind::Memory);
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!("memory for `{TO}` already exists; merge it by hand or abandon the rename")
    );
    assert_eq!(memory_rows(&after, FROM).await, 2);
    assert_eq!(memory_rows(&after, TO).await, 1);
    assert_eq!(records(&after).len(), 1);
}

#[tokio::test]
async fn abandoning_a_rename_drops_only_its_record_and_reports_what_stays() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM, "owl"]);
    let old = seed_workspace(&before, FROM);
    seed_memory(&before, FROM, 2).await;
    seed_cron(&before, FROM);
    let first = arm_and_commit(&before).await;
    let armed = arm(&first, "owl", "wolf").await.unwrap();
    let after = renamed(&first, "owl", "wolf");
    acknowledge_commit(&after, armed).await;

    // Only a record of exactly this rename is abandoned.
    for (from, to) in [(FROM, "wolf"), ("owl", TO), ("hawk", TO)] {
        assert!(
            matches!(
                abandon_rename(&after, from, to).await,
                Err(RenameRecoveryError::NotConfigured { alias }) if alias == from
            ),
            "{from} -> {to}"
        );
    }
    assert_eq!(records(&after).len(), 2);

    let abandoned = abandon_rename(&after, FROM, TO).await.unwrap();
    assert_eq!(
        (abandoned.record.from.as_str(), abandoned.record.to.as_str()),
        (FROM, TO)
    );
    let residue: Vec<(FollowerKind, String)> = abandoned
        .residue
        .iter()
        .map(|issue| (issue.follower, issue.to_string()))
        .collect();
    assert_eq!(
        residue,
        vec![
            (
                FollowerKind::Workspace,
                format!(
                    "the old default workspace of `{FROM}` still exists at {}",
                    old.display()
                )
            ),
            (
                FollowerKind::Memory,
                format!("memory still attributes state to `{FROM}`")
            ),
            (
                FollowerKind::Cron,
                format!("cron still attributes state to `{FROM}`")
            ),
        ]
    );

    // The other rename's record is kept; nothing moved; no config was
    // written; and the old alias can be created again.
    let remaining = records(&after);
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        (remaining[0].from.as_str(), remaining[0].to.as_str()),
        ("owl", "wolf")
    );
    assert!(old.join("MEMORY.md").is_file());
    assert_eq!(memory_identity(&after, FROM).await, 1);
    assert_eq!(cron_residue(&after, FROM), Some(1));
    assert!(!after.default_agent_workspace_dir(TO).exists());
    assert!(!after.config_path.exists());
    assert!(ensure_alias_not_retired(&after, FROM).await.is_ok());
    assert!(ensure_not_pending_target(&after, TO).await.is_ok());
    let mut recreated = after.clone();
    assert!(alias_refs::create_map_key_checked(&mut recreated, "agents", FROM).unwrap());

    assert!(matches!(
        abandon_rename(&after, FROM, TO).await,
        Err(RenameRecoveryError::NotConfigured { .. })
    ));
}

#[tokio::test]
async fn a_rename_whose_old_alias_is_configured_again_can_be_abandoned() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    let mut readded = after.clone();
    readded.agents.insert(FROM.to_string(), agent());

    let abandoned = abandon_rename(&readded, FROM, TO).await.unwrap();
    assert_eq!(abandoned.residue.len(), 1, "{:?}", abandoned.residue);
    assert_eq!(abandoned.residue[0].follower, FollowerKind::Workspace);
    assert!(records(&after).is_empty());
    assert!(old.join("MEMORY.md").is_file());
    assert_eq!(
        resolve(&readded, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap(),
        Disposition::Fresh,
        "the agent brought back is an ordinary agent again"
    );
}

#[tokio::test]
async fn a_legacy_alias_that_names_more_than_one_directory_is_refused() {
    let tmp = TempDir::new().unwrap();
    let stores = SurfaceStores::none();
    for legacy in ["x/../../outside", "..", ".", "back\\slash", "nul\0byte", ""] {
        let config = fixture(&tmp, &[legacy]);
        let refusals = [
            resolve(&config, legacy, TO, &stores).await.unwrap_err(),
            arm(&config, legacy, TO).await.unwrap_err(),
            converge(&config, legacy, TO, &stores).await.unwrap_err(),
            abandon_rename(&config, legacy, TO).await.unwrap_err(),
        ];
        for refusal in refusals {
            assert!(
                matches!(&refusal, RenameRecoveryError::InvalidAlias { alias, .. } if alias == legacy),
                "{legacy:?}: {refusal:?}"
            );
        }
        assert!(!config.data_dir.exists(), "{legacy:?}: no journal or lock");
    }
    assert!(!tmp.path().join("agents").exists());
    // `<install>/agents/x/../../outside` is `<install>/outside`.
    assert!(!tmp.path().join("outside").exists());
}

#[tokio::test]
async fn a_blocked_destination_keeps_the_record_until_the_workspace_settles() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    seed_cron(&before, FROM);
    seed_acp(&before, "acp-1", FROM, &source);
    let after = arm_and_commit(&before).await;
    let blocker = block_destination(&after);
    let stores = SurfaceStores::none();

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert_eq!(
        lagging_followers(&outcome),
        vec![FollowerKind::Workspace, FollowerKind::Acp]
    );
    assert_eq!(outstanding(&outcome)[0].kind, FollowerIssueKind::Unreadable);
    assert_eq!(
        outstanding(&outcome)[1].to_string(),
        format!(
            "saved session working directories still point at the old default workspace of `{FROM}`"
        ),
        "the session changed owner, but not its working directory"
    );
    assert_eq!(outcome.warnings().len(), 2);
    assert!(!outcome.report().workspace_moved);
    assert_eq!(outcome.report().cron_jobs, 1, "the other followers moved");
    assert_eq!(
        acp_row(&after, "acp-1"),
        (TO.to_string(), source.clone()),
        "the session keeps the directory the workspace is still in"
    );
    assert!(source.exists());
    assert_eq!(records(&after).len(), 1, "the record stays open");

    // Clearing the leftovers by hand does not clear the record.
    std::fs::remove_file(&blocker).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    assert!(retired_alias(&after, FROM).unwrap().is_some());

    let retried = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(retried.is_converged(), "{:?}", retried.warnings());
    assert_eq!(retried.report().acp_workspaces, 1);
    assert_eq!(
        acp_row(&after, "acp-1"),
        (TO.to_string(), after.default_agent_workspace_dir(TO))
    );
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

#[tokio::test]
async fn a_non_empty_destination_conflicts_and_an_empty_one_is_replaced() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    let destination = after.default_agent_workspace_dir(TO);
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.md"), "theirs").unwrap();
    let stores = SurfaceStores::none();

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues = outstanding(&outcome);
    assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
    assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        issues[0].to_string(),
        format!(
            "workspace destination {} already exists and is not empty",
            destination.display()
        )
    );
    assert!(source.exists());
    assert_eq!(
        std::fs::read_to_string(destination.join("keep.md")).unwrap(),
        "theirs"
    );

    std::fs::remove_file(destination.join("keep.md")).unwrap();
    let retried = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(retried.is_converged(), "{:?}", retried.warnings());
    assert!(retried.report().workspace_moved);
    assert_eq!(
        std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
        FROM
    );
    assert!(!source.exists());
}

#[tokio::test]
async fn an_open_record_guards_both_of_its_aliases() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let after = arm_and_commit(&before).await;
    let stores = SurfaceStores::none();

    // Renaming the pending target away, or onto the retired alias, or the
    // retired alias anywhere else, is refused.
    let pending = resolve(&after, TO, "wolf", &stores).await.unwrap_err();
    assert!(
        matches!(&pending, RenameRecoveryError::RecoveryPending { from, to } if from == FROM && to == TO),
        "{pending:?}"
    );
    assert!(!pending.to_string().contains(FROM), "{pending}");
    let onto = resolve(&after, "owl", FROM, &stores).await.unwrap_err();
    assert!(
        matches!(&onto, RenameRecoveryError::AliasRetired { alias, pending_to } if alias == FROM && pending_to == TO),
        "{onto:?}"
    );
    assert!(!onto.to_string().contains(TO), "{onto}");
    let elsewhere = resolve(&after, FROM, "wolf", &stores).await.unwrap_err();
    assert!(
        matches!(&elsewhere, RenameRecoveryError::AliasRetired { alias, .. } if alias == FROM),
        "{elsewhere:?}"
    );

    // Renaming another agent into the pending target is refused as well.
    let mut with_owl = after.clone();
    with_owl.agents.insert("owl".to_string(), agent());
    assert!(matches!(
        resolve(&with_owl, "owl", TO, &stores).await,
        Err(RenameRecoveryError::RecoveryPending { .. })
    ));

    // The creation and deletion guards agree, and so do arm and converge.
    assert!(matches!(
        ensure_not_pending_target(&after, TO).await,
        Err(RenameRecoveryError::RecoveryPending { .. })
    ));
    assert!(matches!(
        ensure_alias_not_retired(&after, FROM).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));
    assert!(ensure_alias_not_retired(&after, TO).await.is_ok());
    assert!(ensure_not_pending_target(&after, FROM).await.is_ok());
    assert!(ensure_alias_not_retired(&after, "owl").await.is_ok());
    // Arming from the config before the commit, where the retired alias is
    // still configured, is refused as a reconfigured old alias.
    assert!(matches!(
        arm(&before, FROM, "wolf").await,
        Err(RenameRecoveryError::SourceReconfigured { .. })
    ));
    assert!(matches!(
        converge(&after, FROM, "wolf", &stores).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));
    assert_eq!(records(&after).len(), 1, "no refusal touched the record");
}

#[tokio::test]
async fn an_abandoned_rename_leaves_no_record_and_moves_nothing() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    // The config file still holds the old alias: the save never landed.
    before.save().await.unwrap();

    let armed = arm(&before, FROM, TO).await.unwrap();
    // While one surface is between its arm and its commit, no other can arm.
    assert!(matches!(
        arm(&before, FROM, "wolf").await,
        Err(RenameRecoveryError::Busy { .. })
    ));
    abandon(&before, armed).await;

    assert!(records(&before).is_empty());
    assert!(!AgentRecoveryJournal::for_config(&before).path().exists());
    assert!(source.exists());
    assert_eq!(retired_alias(&before, FROM).unwrap(), None);
    assert_eq!(
        resolve(&before, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap(),
        Disposition::Fresh
    );
}

#[tokio::test]
async fn acknowledge_and_abandon_defer_to_the_config_they_are_given() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);

    // Acknowledged against a config still holding the old alias: the
    // record stays prepared and retires nothing.
    let armed = arm(&before, FROM, TO).await.unwrap();
    acknowledge_commit(&before, armed).await;
    assert_eq!(records(&before)[0].phase, RecoveryPhase::Prepared);
    assert_eq!(retired_alias(&before, FROM).unwrap(), None);

    // Abandoned against a config that shows the commit: the record stays,
    // so the rename can still be finished.
    let armed = arm(&before, FROM, TO).await.unwrap();
    let after = committed(&before);
    abandon(&after, armed).await;
    assert_eq!(records(&after).len(), 1);
    assert!(retired_alias(&after, FROM).unwrap().is_some());
}

#[tokio::test]
async fn abandoning_a_rename_whose_save_landed_keeps_its_record_committed() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM, "owl"]);
    let source = seed_workspace(&before, FROM);
    before.save().await.unwrap();
    let armed = arm(&before, FROM, TO).await.unwrap();
    // The save lands and the publication after it fails, so the surface
    // abandons with the config it started from, which still has the old
    // alias and lacks the new one.
    let after = save_renamed(&before, FROM, TO).await;
    assert_eq!(
        commit_on_disk(&before.config_path, FROM, TO),
        CommitOnDisk::Landed
    );
    abandon(&before, armed).await;

    let recorded = records(&before);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(
        (
            recorded[0].from.as_str(),
            recorded[0].to.as_str(),
            recorded[0].phase
        ),
        (FROM, TO, RecoveryPhase::Committed)
    );
    assert_eq!(
        recorded[0].source_workspace.as_deref(),
        Some(source.as_path())
    );
    // Committed, the record is not void beside the stale config either,
    // so another rename armed and abandoned from it leaves it alone.
    let other = arm(&before, "owl", "wolf").await.unwrap();
    abandon(&before, other).await;
    assert_eq!(records(&before), recorded);

    // Once the config is loaded from the file again, the old alias stays
    // retired and re-running the same rename finishes it.
    assert!(retired_alias(&after, FROM).unwrap().is_some());
    assert!(matches!(
        ensure_alias_not_retired(&after, FROM).await,
        Err(RenameRecoveryError::AliasRetired { .. })
    ));
    let stores = SurfaceStores::none();
    assert_eq!(
        resolve(&after, FROM, TO, &stores).await.unwrap(),
        Disposition::Resume
    );
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(outcome.report().workspace_moved);
    assert!(!source.exists());
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn abandoning_keeps_the_record_when_the_config_file_cannot_be_read() {
    for case in ["missing", "a directory", "not toml", "agents not a table"] {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        match case {
            "missing" => {}
            "a directory" => std::fs::create_dir_all(&before.config_path).unwrap(),
            "not toml" => std::fs::write(&before.config_path, "[agents.scout\n").unwrap(),
            _ => std::fs::write(&before.config_path, "agents = \"scout\"\n").unwrap(),
        }
        let armed = arm(&before, FROM, TO).await.unwrap();
        abandon(&before, armed).await;

        // Whether the save landed is unknown, so the record is kept as
        // it was: prepared, and void while the config has the old alias
        // and lacks the new one, so it retires nothing.
        let recorded = records(&before);
        assert_eq!(recorded.len(), 1, "{case}: {recorded:?}");
        assert_eq!(recorded[0].phase, RecoveryPhase::Prepared, "{case}");
        assert_eq!(retired_alias(&before, FROM).unwrap(), None, "{case}");
        assert!(
            ensure_alias_not_retired(&before, FROM).await.is_ok(),
            "{case}"
        );
        assert_eq!(
            resolve(&before, FROM, TO, &SurfaceStores::none())
                .await
                .unwrap(),
            Disposition::Fresh,
            "{case}"
        );
    }
}

#[test]
fn the_commit_on_disk_is_read_off_the_parsed_agent_keys() {
    // A legacy alias that TOML accepts only as a quoted key.
    const QUOTED: &str = "Old Agent";
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("config.toml");
    let read = |text: &str| {
        std::fs::write(&path, text).unwrap();
        commit_on_disk(&path, QUOTED, MIGRATED)
    };

    for landed in [
        "[agents.legacy_agent]\nenabled = true\n",
        "[agents.\"legacy_agent\"]\n",
        "[agents]\nlegacy_agent = { enabled = true }\n",
        "agents.legacy_agent.enabled = true\n",
        "# [agents.\"Old Agent\"]\n[agents.'legacy_agent']\n",
    ] {
        assert_eq!(read(landed), CommitOnDisk::Landed, "{landed}");
    }
    for absent in [
        "[agents.\"Old Agent\"]\n",
        "[agents.'Old Agent']\n[agents.legacy_agent]\n",
        "[agents]\n\"Old Agent\" = { enabled = true }\nlegacy_agent = {}\n",
        "[agents]\n",
        "schema_version = 3\n# [agents.legacy_agent]\n",
    ] {
        assert_eq!(read(absent), CommitOnDisk::Absent, "{absent}");
    }
    for unknown in [
        "[agents.\"Old Agent\"\n",
        "agents = [\"legacy_agent\"]\n",
        "api_key = \"sk-not-for-logs\n[agents.legacy_agent]\n",
    ] {
        match read(unknown) {
            CommitOnDisk::Unknown(detail) => assert!(
                !detail.contains("sk-not-for-logs"),
                "the file's contents stay out of the detail: {detail}"
            ),
            other => panic!("{unknown}: {other:?}"),
        }
    }
    assert!(matches!(
        commit_on_disk(&tmp.path().join("missing.toml"), QUOTED, MIGRATED),
        CommitOnDisk::Unknown(_)
    ));
}

#[tokio::test]
async fn converge_refuses_a_rename_whose_commit_never_landed() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let stores = SurfaceStores::none();

    assert!(matches!(
        converge(&before, FROM, TO, &stores).await,
        Err(RenameRecoveryError::InvalidAlias { .. })
    ));
    let armed = arm(&before, FROM, TO).await.unwrap();
    assert!(matches!(
        converge(&before, FROM, TO, &stores).await,
        Err(RenameRecoveryError::InvalidAlias { .. })
    ));
    abandon(&before, armed).await;
    assert!(source.exists());
}

#[tokio::test]
async fn acp_rows_follow_every_spelling_of_the_moved_workspace() {
    let tmp = TempDir::new().unwrap();
    let real_install = tmp.path().join("install");
    std::fs::create_dir_all(&real_install).unwrap();
    // On unix the install root is reached through a symlink, so a row that
    // recorded the resolved path spells the workspace differently. Windows
    // resolves to a verbatim `\\?\` path, which differs as well.
    #[cfg(unix)]
    let install = {
        let link = tmp.path().join("install-link");
        std::os::unix::fs::symlink(&real_install, &link).unwrap();
        link
    };
    #[cfg(not(unix))]
    let install = real_install;
    let mut before = fixture(&tmp, &[FROM]);
    before.config_path = install.join("config.toml");
    let source = seed_workspace(&before, FROM);
    let canonical = std::fs::canonicalize(&source).unwrap();
    assert_ne!(canonical, source, "the workspace must have two spellings");
    let custom = tmp.path().join("elsewhere");
    seed_acp(&before, "raw", FROM, &source);
    seed_acp(&before, "canonical", FROM, &canonical);
    seed_acp(&before, "nested", "other", &source.join("proj"));
    seed_acp(&before, "custom", FROM, &custom);

    let after = arm_and_commit(&before).await;
    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().acp_sessions, 3);
    assert_eq!(outcome.report().acp_workspaces, 3);

    let destination = after.default_agent_workspace_dir(TO);
    assert_eq!(
        acp_row(&after, "raw"),
        (TO.to_string(), destination.clone())
    );
    assert_eq!(
        acp_row(&after, "canonical"),
        (TO.to_string(), destination.clone())
    );
    assert_eq!(
        acp_row(&after, "nested"),
        ("other".to_string(), destination.join("proj"))
    );
    assert_eq!(acp_row(&after, "custom"), (TO.to_string(), custom));
}

#[cfg(unix)]
#[tokio::test]
async fn acp_rows_follow_a_workspace_an_earlier_run_already_moved() {
    let tmp = TempDir::new().unwrap();
    let real_install = tmp.path().join("install");
    std::fs::create_dir_all(&real_install).unwrap();
    let install = tmp.path().join("install-link");
    std::os::unix::fs::symlink(&real_install, &install).unwrap();
    let mut before = fixture(&tmp, &[FROM]);
    before.config_path = install.join("config.toml");
    let source = seed_workspace(&before, FROM);
    let canonical = std::fs::canonicalize(&source).unwrap();
    seed_acp(&before, "canonical", FROM, &canonical.join("proj"));
    let after = arm_and_commit(&before).await;

    // An earlier run moved the workspace and removed the old alias
    // directory, then stopped before the ACP rows followed.
    let destination = after.default_agent_workspace_dir(TO);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::rename(&source, &destination).unwrap();
    std::fs::remove_dir(source.parent().unwrap()).unwrap();

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert!(!outcome.report().workspace_moved);
    assert_eq!(
        acp_row(&after, "canonical"),
        (TO.to_string(), destination.join("proj"))
    );
}

/// A recorded rename whose first converge stopped on a non-empty
/// destination, after which the operator gave the new alias the custom
/// workspace `custom`. ACP sessions were saved in the old default
/// workspace, beneath it, and in `unrelated`. Returns the config with the
/// custom path, and the old default workspace, which still holds its
/// files.
async fn conflicted_rename_given_a_custom_workspace(
    tmp: &TempDir,
    custom: &Path,
    unrelated: &Path,
) -> (Config, PathBuf) {
    let before = fixture(tmp, &[FROM]);
    let old = seed_workspace(&before, FROM);
    std::fs::create_dir(old.join("sub")).unwrap();
    seed_acp(&before, "at-old", FROM, &old);
    seed_acp(&before, "under-old", FROM, &old.join("sub"));
    seed_acp(&before, "unrelated", "other", unrelated);
    let mut after = arm_and_commit(&before).await;
    let destination = after.default_agent_workspace_dir(TO);
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("keep.md"), "theirs").unwrap();

    let first = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert_eq!(outstanding(&first)[0].kind, FollowerIssueKind::Conflict);
    assert_eq!(
        acp_row(&after, "at-old"),
        (TO.to_string(), old.clone()),
        "the session changed owner, but stays with the files that did not move"
    );
    after.agents.get_mut(TO).unwrap().workspace.path = Some(custom.to_path_buf());
    (after, old)
}

#[tokio::test]
async fn saved_sessions_follow_files_moved_by_hand_to_a_custom_workspace() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("custom");
    let unrelated = tmp.path().join("unrelated");
    let (after, old) = conflicted_rename_given_a_custom_workspace(&tmp, &custom, &unrelated).await;
    // The operator moves the old files into the custom workspace by hand,
    // which leaves no old default workspace behind, then re-runs the
    // rename.
    std::fs::rename(&old, &custom).unwrap();

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().acp_workspaces, 2);
    assert_eq!(acp_row(&after, "at-old"), (TO.to_string(), custom.clone()));
    assert_eq!(
        acp_row(&after, "under-old"),
        (TO.to_string(), custom.join("sub"))
    );
    assert_eq!(
        acp_row(&after, "unrelated"),
        ("other".to_string(), unrelated)
    );
    assert!(custom.join("MEMORY.md").is_file());
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

#[tokio::test]
async fn saved_sessions_stay_with_files_left_in_the_old_workspace() {
    let tmp = TempDir::new().unwrap();
    let custom = tmp.path().join("custom");
    let unrelated = tmp.path().join("unrelated");
    let (after, old) = conflicted_rename_given_a_custom_workspace(&tmp, &custom, &unrelated).await;

    // Re-run before the old files have moved: a resumed session still
    // belongs with them, and the old alias stays retired.
    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    let issues: Vec<(FollowerKind, FollowerIssueKind, String)> = outstanding(&outcome)
        .iter()
        .map(|issue| (issue.follower, issue.kind, issue.to_string()))
        .collect();
    assert_eq!(
        issues,
        vec![
            (
                FollowerKind::Workspace,
                FollowerIssueKind::Conflict,
                format!(
                    "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
                    old.display()
                ),
            ),
            (
                FollowerKind::Acp,
                FollowerIssueKind::Lagging,
                format!(
                    "saved session working directories still point at the old default workspace of `{FROM}`"
                ),
            ),
        ]
    );
    assert_eq!(outcome.report().acp_workspaces, 0);
    assert_eq!(acp_row(&after, "at-old"), (TO.to_string(), old.clone()));
    assert_eq!(
        acp_row(&after, "under-old"),
        (TO.to_string(), old.join("sub"))
    );
    assert_eq!(
        acp_row(&after, "unrelated"),
        ("other".to_string(), unrelated)
    );
    assert!(old.join("MEMORY.md").is_file());
    assert_eq!(records(&after).len(), 1);
    assert!(retired_alias(&after, FROM).unwrap().is_some());
}

#[tokio::test]
async fn saved_sessions_stay_while_the_new_workspace_overlaps_the_old_one() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = before.default_agent_workspace_dir(FROM);
    let unrelated = tmp.path().join("unrelated");
    seed_acp(&before, "at-old", FROM, &old);
    seed_acp(&before, "under-old", FROM, &old.join("sub"));
    seed_acp(&before, "unrelated", "other", &unrelated);
    let mut after = arm_and_commit(&before).await;
    // Nothing is at the old default workspace, so only the overlap keeps
    // the saved working directories where they are.
    after.agents.get_mut(TO).unwrap().workspace.path = Some(old.join("proj"));
    assert!(!old.exists());
    let stores = SurfaceStores::none();

    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    let issues: Vec<(FollowerKind, String)> = outstanding(&outcome)
        .iter()
        .map(|issue| (issue.follower, issue.to_string()))
        .collect();
    assert_eq!(
        issues,
        vec![
            (
                FollowerKind::Workspace,
                format!(
                    "the workspace.path of agent `{TO}` is or overlaps the old default workspace of `{FROM}`; point workspace.path elsewhere, then re-run the rename"
                ),
            ),
            (
                FollowerKind::Acp,
                format!(
                    "saved session working directories still point at the old default workspace of `{FROM}`"
                ),
            ),
        ],
        "no relocation into the overlapping workspace was attempted"
    );
    assert_eq!(outcome.report().acp_sessions, 2, "the owner still follows");
    assert_eq!(outcome.report().acp_workspaces, 0);
    assert_eq!(acp_row(&after, "at-old"), (TO.to_string(), old.clone()));
    assert_eq!(
        acp_row(&after, "under-old"),
        (TO.to_string(), old.join("sub"))
    );
    assert_eq!(
        acp_row(&after, "unrelated"),
        ("other".to_string(), unrelated.clone())
    );
    assert_eq!(records(&after).len(), 1);

    // Pointed elsewhere, the next run re-points them and finishes.
    let elsewhere = tmp.path().join("elsewhere");
    after.agents.get_mut(TO).unwrap().workspace.path = Some(elsewhere.clone());
    let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().acp_workspaces, 2);
    assert_eq!(
        acp_row(&after, "at-old"),
        (TO.to_string(), elsewhere.clone())
    );
    assert_eq!(
        acp_row(&after, "under-old"),
        (TO.to_string(), elsewhere.join("sub"))
    );
    assert_eq!(
        acp_row(&after, "unrelated"),
        ("other".to_string(), unrelated)
    );
    assert!(records(&after).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn saved_sessions_stay_while_the_new_workspace_overlaps_the_old_one_through_a_symlink() {
    let tmp = TempDir::new().unwrap();
    let (old, linked_old) = workspace_through_a_link(&tmp);
    let mut before = fixture(&tmp, &[FROM]);
    before.config_path = tmp.path().join("install").join("config.toml");
    assert_eq!(before.default_agent_workspace_dir(FROM), old);
    seed_acp(&before, "at-old", FROM, &old);
    seed_acp(&before, "under-old", FROM, &old.join("sub"));
    let mut after = arm_and_commit(&before).await;
    // Neither workspace exists, and the new one is spelled through the
    // link, so only resolving the link shows that it lies in the old one.
    let custom = linked_old.join("proj");
    after.agents.get_mut(TO).unwrap().workspace.path = Some(custom.clone());

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    let issues: Vec<(FollowerKind, String)> = outstanding(&outcome)
        .iter()
        .map(|issue| (issue.follower, issue.to_string()))
        .collect();
    assert_eq!(
        issues,
        vec![
            (
                FollowerKind::Workspace,
                format!(
                    "the workspace.path of agent `{TO}` is or overlaps the old default workspace of `{FROM}`; point workspace.path elsewhere, then re-run the rename"
                ),
            ),
            (
                FollowerKind::Acp,
                format!(
                    "saved session working directories still point at the old default workspace of `{FROM}`"
                ),
            ),
        ],
        "no relocation into the overlapping workspace was attempted"
    );
    assert_eq!(outcome.report().acp_sessions, 2, "the owner still follows");
    assert_eq!(outcome.report().acp_workspaces, 0);
    assert_eq!(acp_row(&after, "at-old"), (TO.to_string(), old.clone()));
    assert_eq!(
        acp_row(&after, "under-old"),
        (TO.to_string(), old.join("sub"))
    );
    assert_eq!(records(&after).len(), 1);
    assert!(!old.exists());
    assert!(!custom.exists());
}

#[tokio::test]
async fn saved_sessions_outside_the_old_workspace_are_never_touched() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let old = before.default_agent_workspace_dir(FROM);
    // One session of the old alias's saved elsewhere, and one of another
    // agent's in a sibling whose name merely starts like the old default
    // workspace; beside them, one saved in the old default workspace.
    let unrelated = tmp.path().join("unrelated");
    let sibling = old.parent().unwrap().join("workspace2");
    seed_acp(&before, "unrelated", FROM, &unrelated);
    seed_acp(&before, "sibling", "other", &sibling);
    seed_acp(&before, "at-old", "other", &old);
    let mut after = arm_and_commit(&before).await;
    let custom = tmp.path().join("custom");
    after.agents.get_mut(TO).unwrap().workspace.path = Some(custom.clone());

    let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().acp_sessions, 1);
    assert_eq!(outcome.report().acp_workspaces, 1);
    assert_eq!(acp_row(&after, "unrelated"), (TO.to_string(), unrelated));
    assert_eq!(acp_row(&after, "sibling"), ("other".to_string(), sibling));
    assert_eq!(acp_row(&after, "at-old"), ("other".to_string(), custom));
    assert!(records(&after).is_empty());
}

#[tokio::test]
async fn converging_a_converged_rename_again_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    seed_every_follower(&before).await;
    let after = arm_and_commit(&before).await;
    let stores = SurfaceStores::none();
    let first = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(first.is_converged(), "{:?}", first.warnings());
    let destination = after.default_agent_workspace_dir(TO);

    let again = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(again.is_converged(), "{:?}", again.warnings());
    let report = again.report();
    assert!(!report.workspace_moved);
    assert_eq!(
        (
            report.memory_rows,
            report.cron_jobs,
            report.acp_sessions,
            report.acp_workspaces,
            report.sessions_repointed,
        ),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
        FROM
    );
    assert_eq!(memory_rows(&after, TO).await, 2);
    assert_eq!(cron_residue(&after, TO), Some(1));
    assert_eq!(acp_row(&after, "acp-1"), (TO.to_string(), destination));
    assert_eq!(session_owner(&after, "chat-1").as_deref(), Some(TO));
    assert!(records(&after).is_empty());
}

/// The directories a workspace move or cleanup of the fixture rename
/// changes: `<install>/agents/<TO>`, `<install>/agents`, and
/// `<install>/agents/<FROM>`.
fn workspace_parent_dirs(config: &Config) -> (PathBuf, PathBuf, PathBuf) {
    let new_alias_dir = config
        .default_agent_workspace_dir(TO)
        .parent()
        .unwrap()
        .to_path_buf();
    let agents_dir = new_alias_dir.parent().unwrap().to_path_buf();
    let old_alias_dir = config
        .default_agent_workspace_dir(FROM)
        .parent()
        .unwrap()
        .to_path_buf();
    (new_alias_dir, agents_dir, old_alias_dir)
}

fn assert_unsynced(outcome: &ConvergeOutcome, dir: &Path) {
    let issues = outstanding(outcome);
    assert_eq!(
        issues.len(),
        1,
        "{}: {:?}",
        dir.display(),
        outcome.warnings()
    );
    assert_eq!(issues[0].follower, FollowerKind::Workspace);
    assert_eq!(issues[0].kind, FollowerIssueKind::Unreadable);
    assert!(
        issues[0]
            .detail
            .starts_with(&format!("could not sync {}: ", dir.display())),
        "{}",
        issues[0]
    );
}

#[tokio::test]
async fn a_failed_directory_sync_keeps_the_record_until_a_rerun_syncs_it() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    let (_, agents_dir, old_alias_dir) = workspace_parent_dirs(&after);
    let destination = after.default_agent_workspace_dir(TO);
    let stores = SurfaceStores::none();

    // The move lands, but the directory that gained the new alias and
    // lost the old one cannot be synced, so a power loss could still
    // undo it.
    let first = with_failing_sync(agents_dir.clone(), converge(&after, FROM, TO, &stores))
        .await
        .unwrap();
    assert_unsynced(&first, &agents_dir);
    assert!(first.report().workspace_moved);
    assert!(destination.join("MEMORY.md").is_file());
    assert!(!source.exists());
    assert!(!old_alias_dir.exists());
    assert_eq!(records(&after).len(), 1, "the record stays open");
    assert!(retired_alias(&after, FROM).unwrap().is_some());

    // A re-run has nothing left to move, syncs again, and clears it.
    let second = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(second.is_converged(), "{:?}", second.warnings());
    assert!(!second.report().workspace_moved);
    assert!(destination.join("MEMORY.md").is_file());
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

#[tokio::test]
async fn a_rename_resumed_after_an_unsynced_move_syncs_before_clearing() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    let source = seed_workspace(&before, FROM);
    let after = arm_and_commit(&before).await;
    let (new_alias_dir, agents_dir, old_alias_dir) = workspace_parent_dirs(&after);
    let destination = after.default_agent_workspace_dir(TO);
    // A run moved the workspace and stopped before syncing anything or
    // removing the old alias directory; its record is still committed.
    std::fs::create_dir_all(&new_alias_dir).unwrap();
    std::fs::rename(&source, &destination).unwrap();
    assert_eq!(records(&after)[0].phase, RecoveryPhase::Committed);
    let stores = SurfaceStores::none();

    // Resuming moves nothing, yet syncs every directory the move changed.
    for dir in [&new_alias_dir, &agents_dir, &old_alias_dir] {
        let resumed = with_failing_sync(dir.clone(), converge(&after, FROM, TO, &stores))
            .await
            .unwrap();
        assert_unsynced(&resumed, dir);
        assert!(!resumed.report().workspace_moved);
        assert_eq!(records(&after).len(), 1, "{}", dir.display());
    }

    let finished = converge(&after, FROM, TO, &stores).await.unwrap();
    assert!(finished.is_converged(), "{:?}", finished.warnings());
    assert!(!finished.report().workspace_moved);
    assert!(destination.join("MEMORY.md").is_file());
    assert!(records(&after).is_empty());
    assert_eq!(retired_alias(&after, FROM).unwrap(), None);
}

#[tokio::test]
async fn a_directory_that_does_not_exist_is_not_synced() {
    let tmp = TempDir::new().unwrap();
    let before = fixture(&tmp, &[FROM]);
    // Only cron state follows, so nothing under `<install>/agents` was
    // ever created.
    seed_cron(&before, FROM);
    let after = arm_and_commit(&before).await;
    let (new_alias_dir, agents_dir, old_alias_dir) = workspace_parent_dirs(&after);
    assert!(!agents_dir.exists());
    let stores = SurfaceStores::none();

    let outcome = with_failing_sync(agents_dir.clone(), converge(&after, FROM, TO, &stores))
        .await
        .unwrap();
    assert!(outcome.is_converged(), "{:?}", outcome.warnings());
    assert_eq!(outcome.report().cron_jobs, 1);
    assert!(records(&after).is_empty());
    for dir in [new_alias_dir, agents_dir, old_alias_dir] {
        assert!(!dir.exists(), "{} was created", dir.display());
    }
}

#[tokio::test]
async fn a_corrupt_journal_fails_every_entry_point_closed() {
    let tmp = TempDir::new().unwrap();
    let config = fixture(&tmp, &[FROM]);
    let journal = AgentRecoveryJournal::for_config(&config);
    std::fs::create_dir_all(&config.data_dir).unwrap();
    std::fs::write(journal.path(), "{not json").unwrap();
    let stores = SurfaceStores::none();

    let err = resolve(&config, FROM, TO, &stores).await.unwrap_err();
    assert!(
        matches!(&err, RenameRecoveryError::Unreadable { store, .. } if store == JOURNAL_STORE),
        "{err:?}"
    );
    assert!(matches!(
        arm(&config, FROM, TO).await,
        Err(RenameRecoveryError::Unreadable { .. })
    ));
    assert!(matches!(
        converge(&config, FROM, TO, &stores).await,
        Err(RenameRecoveryError::Unreadable { .. })
    ));
    assert!(matches!(
        ensure_alias_not_retired(&config, FROM).await,
        Err(RenameRecoveryError::Unreadable { .. })
    ));
    assert!(matches!(
        ensure_not_pending_target(&config, TO).await,
        Err(RenameRecoveryError::Unreadable { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(journal.path()).unwrap(),
        "{not json"
    );
}

#[test]
fn warning_lines_and_errors_keep_their_shapes() {
    let names: Vec<String> = FollowerKind::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["workspace", "memory", "cron", "acp", "sessions"]);

    let unreadable = FollowerIssue::unreadable(FollowerKind::Cron, "not a database".into());
    assert_eq!(
        unreadable.to_string(),
        "cron could not be read: not a database"
    );
    assert_eq!(
        serde_json::to_value(&unreadable).unwrap(),
        serde_json::json!({
            "follower": "cron",
            "kind": "unreadable",
            "detail": "not a database",
        })
    );
    let lagging = FollowerIssue::lagging(FollowerKind::Memory, "memory rename: boom".into());
    assert_eq!(lagging.to_string(), "memory rename: boom");

    let outcome = ConvergeOutcome::Incomplete {
        report: ConvergeReport::default(),
        outstanding: vec![unreadable, lagging],
    };
    assert!(!outcome.is_converged());
    assert_eq!(
        outcome.warnings(),
        [
            "cron could not be read: not a database",
            "memory rename: boom"
        ]
    );

    let error = |e: RenameRecoveryError| e.to_string();
    assert_eq!(
        error(RenameRecoveryError::NotConfigured { alias: FROM.into() }),
        "agents.scout is not configured"
    );
    assert_eq!(
        error(RenameRecoveryError::AliasRetired {
            alias: FROM.into(),
            pending_to: TO.into(),
        }),
        "alias `scout` is retired by an unfinished agent rename and cannot be reused yet"
    );
    assert_eq!(
        error(RenameRecoveryError::RecoveryPending {
            from: FROM.into(),
            to: TO.into(),
        }),
        "agent `ranger` is the target of an unfinished rename; re-run that rename first"
    );
    assert_eq!(
        error(RenameRecoveryError::SourceReconfigured {
            from: FROM.into(),
            to: TO.into(),
        }),
        "agent `scout` is configured again while an earlier rename of it is unfinished; remove `[agents.scout]` from the config by hand, or abandon that rename, then retry"
    );
    let conflict = FollowerIssue::conflict(FollowerKind::Memory, "taken".into());
    assert_eq!(conflict.to_string(), "taken");
    assert_eq!(
        serde_json::to_value(&conflict).unwrap()["kind"],
        serde_json::json!("conflict")
    );
    assert_eq!(
        error(RenameRecoveryError::Unreadable {
            store: "acp".into(),
            detail: "locked".into(),
        }),
        "acp could not be read: locked"
    );
    assert_eq!(
        error(RenameRecoveryError::Busy {
            detail: "held".into()
        }),
        "agent rename recovery is in progress elsewhere; retry shortly (held)"
    );
    assert_eq!(
        error(RenameRecoveryError::Persist {
            detail: "full".into()
        }),
        "agent rename recovery could not be recorded: full"
    );
}

/// Surfaces await these inside handler futures with a 16 KiB budget, so
/// each must be `Send` and leave most of that budget to the handler.
#[test]
fn entry_points_are_send_and_their_futures_stay_small() {
    fn send<T: Send>(value: T) -> T {
        value
    }
    send(Option::<Armed>::None);
    let config = Config::default();
    let stores = SurfaceStores::none();
    let sizes = [
        (
            "resolve",
            std::mem::size_of_val(&send(resolve(&config, FROM, TO, &stores))),
        ),
        ("arm", std::mem::size_of_val(&send(arm(&config, FROM, TO)))),
        (
            "converge",
            std::mem::size_of_val(&send(converge(&config, FROM, TO, &stores))),
        ),
        (
            "ensure_alias_not_retired",
            std::mem::size_of_val(&send(ensure_alias_not_retired(&config, FROM))),
        ),
        (
            "ensure_not_pending_target",
            std::mem::size_of_val(&send(ensure_not_pending_target(&config, TO))),
        ),
        (
            "abandon_rename",
            std::mem::size_of_val(&send(abandon_rename(&config, FROM, TO))),
        ),
    ];
    for (name, size) in sizes {
        assert!(size <= 4 * 1024, "the {name} future is {size} bytes");
    }
}
