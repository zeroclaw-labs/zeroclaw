//! Agent rename/delete **owned-state** cascades: the non-config half of
//! changing an agent alias lifecycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tokio::io::AsyncWriteExt;
use zeroclaw_api::attribution::{Attributable, MemoryKind, Role};
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

pub fn live_acp_session_count(config: &Config, alias: &str) -> anyhow::Result<usize> {
    let store = AcpSessionStore::new(&config.data_dir)
        .context("open ACP session store to verify live sessions")?;
    store
        .count_live_sessions_by_agent(alias)
        .context("count live ACP sessions for agent")
}

fn resolve_memory_for_owned_state(
    config: &Config,
    memory: Option<&Arc<dyn Memory>>,
) -> anyhow::Result<Option<Arc<dyn Memory>>> {
    // An already-open handle may still contain rows after a live config toggle;
    // preserve the pre-existing cleanup behavior instead of stranding those
    // rows when the currently selected backend is `none`.
    let configured_backend = zeroclaw_memory::backend_kind_from_dotted(&config.memory.backend);
    let configured_kind = zeroclaw_memory::classify_memory_backend(&configured_backend);

    if let Some(memory) = memory
        && (!matches!(memory.role(), Role::Memory(MemoryKind::None))
            || matches!(configured_kind, zeroclaw_memory::MemoryBackendKind::None))
    {
        return Ok(Some(Arc::clone(memory)));
    }

    // The gateway deliberately falls back to `NoneMemory` when its configured
    // durable backend cannot be opened. That placeholder keeps unrelated HTTP
    // surfaces alive, but it is not evidence that owned memory is empty. When
    // config still expects persistence, ignore the placeholder and reopen the
    // canonical configured backend so deletion either cleans it or fails toward
    // a later retry.
    if matches!(configured_kind, zeroclaw_memory::MemoryBackendKind::None) {
        return Ok(None);
    }

    zeroclaw_memory::create_memory_from_config(config, None)
        .map(Arc::from)
        .map(Some)
        .context("open configured memory backend for owned-state recovery")
}

fn resolve_session_backend_for_owned_state(
    config: &Config,
    session_backend: Option<&Arc<dyn SessionBackend>>,
) -> std::io::Result<Option<Arc<dyn SessionBackend>>> {
    // As with memory, an existing handle is authoritative evidence that a
    // durable store is reachable and may contain stale attribution from before
    // a live config toggle.
    if let Some(session_backend) = session_backend {
        return Ok(Some(Arc::clone(session_backend)));
    }
    // Gateway WebSocket and channel histories share this backend. Only an
    // absent handle with both producers disabled means there is no configured
    // store to recover; otherwise absence means the expected store is
    // unavailable and must fail toward another retry.
    if !config.gateway.session_persistence && !config.channels.session_persistence {
        return Ok(None);
    }

    zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
        .map(Some)
}

/// What a filesystem probe could establish about a lifecycle path.
///
/// The alias lifecycle needs three answers, not two. "Exists" and "does not
/// exist" both let a cascade proceed; "cannot tell" must fail toward residue so
/// the surface stays retryable instead of reporting convergence over state that
/// is still on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPresence {
    /// The path exists.
    Present,
    /// The path is genuinely absent and every component leading to it was
    /// inspectable.
    Absent,
    /// The path could not be inspected. The payload is the operator-visible
    /// reason.
    Uninspectable(String),
}

impl PathPresence {
    /// Is this the fail-toward-residue answer?
    #[must_use]
    pub fn is_uninspectable(&self) -> bool {
        matches!(self, Self::Uninspectable(_))
    }

    /// The operator-visible reason, when the path could not be inspected.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Uninspectable(reason) => Some(reason.as_str()),
            Self::Present | Self::Absent => None,
        }
    }
}

/// Find the component of `path` that stands between the caller and an answer.
///
/// Walks upward from the parent to the first ancestor that exists. A directory
/// there means `path` is genuinely absent. Anything else - a file standing
/// where a directory must be, or an ancestor that cannot be inspected either -
/// means the probe never reached `path` at all.
async fn obstructing_ancestor(path: &Path) -> Option<String> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match tokio::fs::metadata(ancestor).await {
            Ok(metadata) if metadata.is_dir() => return None,
            Ok(_) => {
                return Some(format!("{} is not a directory", ancestor.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Some(format!(
                    "{} could not be inspected: {error}",
                    ancestor.display()
                ));
            }
        }
    }
    None
}

/// Turn one `try_exists` result into a [`PathPresence`], identically on every
/// supported platform.
///
/// `Ok(false)` is not proof of absence. When a component of the path is a file
/// rather than a directory, Unix surfaces `ENOTDIR` as an `Err` while Windows
/// reports the same shape as an ordinary "not found", so a predicate written
/// against the error shape silently flips meaning between platforms. Re-walking
/// the ancestors of a negative probe is what makes the recovery contract the
/// same everywhere: an uninspectable workspace is residue on Windows too.
///
/// Kept separate from [`inspect_lifecycle_path`] so both probe shapes can be
/// driven explicitly from a single host's unit tests.
async fn classify_presence(path: &Path, probe: std::io::Result<bool>) -> PathPresence {
    match probe {
        Ok(true) => PathPresence::Present,
        Err(error) => PathPresence::Uninspectable(error.to_string()),
        Ok(false) => match obstructing_ancestor(path).await {
            Some(reason) => PathPresence::Uninspectable(reason),
            None => PathPresence::Absent,
        },
    }
}

/// Canonical presence probe for every alias-lifecycle path.
///
/// Gateway, CLI and RPC all answer "is this workspace still there" through this
/// one function so that a metadata failure can never be read as absence on one
/// platform and as residue on another.
pub async fn inspect_lifecycle_path(path: &Path) -> PathPresence {
    // Exercise Windows' negative metadata result through real cascades on any
    // test host, without changing probes for unrelated paths or concurrent tests.
    #[cfg(test)]
    if NEGATIVE_PROBE_PATH
        .try_with(|forced| forced == path)
        .unwrap_or(false)
    {
        return classify_presence(path, Ok(false)).await;
    }
    classify_presence(path, tokio::fs::try_exists(path).await).await
}

#[cfg(test)]
tokio::task_local! {
    static NEGATIVE_PROBE_PATH: PathBuf;
}

#[derive(Debug, Clone, Copy)]
enum CommittedLifecycle {
    Delete,
    Rename,
}

/// Does durable owned state still exist for `alias` after its config mutation
/// is already committed?
///
/// Delete and rename share every physical store probe, but memory deliberately
/// has distinct canonical predicates: delete recovery only needs rows that
/// require purging, while rename recovery must also detect the stable agent
/// identity row moved by `Memory::rename_agent` when it owns zero memories.
/// Both modes fail toward residue when state cannot be inspected.
async fn committed_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    lifecycle: CommittedLifecycle,
) -> bool {
    match inspect_lifecycle_path(&config.agent_workspace_dir(alias)).await {
        PathPresence::Present | PathPresence::Uninspectable(_) => return true,
        PathPresence::Absent => {}
    }

    if crate::cron::list_jobs_by_agent(config, alias)
        .map(|jobs| !jobs.is_empty())
        .unwrap_or(true)
    {
        return true;
    }

    if crate::control_plane::SqliteTaskStore::count_existing_by_agent(&config.data_dir, alias)
        .map_or(true, |count| count > 0)
    {
        return true;
    }

    match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => {
            if store
                .list_sessions_by_agent(alias)
                .map(|sessions| !sessions.is_empty())
                .unwrap_or(true)
            {
                return true;
            }
        }
        Err(_) => return true,
    }

    match resolve_memory_for_owned_state(config, mem) {
        Ok(Some(mem)) => {
            let residue = match lifecycle {
                CommittedLifecycle::Delete => mem
                    .export_agent(alias)
                    .await
                    .map_or(true, |rows| !rows.is_empty()),
                CommittedLifecycle::Rename => {
                    mem.count_agent(alias).await.map_or(true, |count| count > 0)
                }
            };
            if residue {
                return true;
            }
        }
        Err(_) => return true,
        Ok(_) => {}
    }

    let knowledge_path = config.knowledge.resolved_db_path();
    match inspect_lifecycle_path(&knowledge_path).await {
        // Recovery probes can run under config-writer serialization. Never
        // initialize/migrate the graph here (live effects lock graph -> config).
        PathPresence::Present => match prepare_knowledge_retirement(config, alias) {
            Ok(Some(snapshot)) => {
                if ["nodes", "edges"].iter().any(|key| {
                    snapshot
                        .get(key)
                        .and_then(serde_json::Value::as_array)
                        .is_none_or(|rows| !rows.is_empty())
                }) {
                    return true;
                }
            }
            Err(_) => return true,
            Ok(None) => {}
        },
        PathPresence::Uninspectable(_) => return true,
        PathPresence::Absent => {}
    }

    match resolve_session_backend_for_owned_state(config, session_backend) {
        Ok(Some(backend)) if backend.count_agent_attribution(alias).unwrap_or(1) > 0 => {
            return true;
        }
        Err(_) => return true,
        Ok(_) => {}
    }

    false
}

/// Does purgeable state still exist for `alias` after its config entry is
/// already gone?
///
/// This is the shared **committed-delete recovery** contract. Every supported
/// alias lifecycle surface (gateway, CLI, RPC) persists the removal of
/// `agents.<alias>` before running the owned-state cascade. A retry re-enters
/// that cascade while owned rows or an unreadable store remain.
pub async fn committed_delete_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
) -> bool {
    committed_residue_exists(
        config,
        mem,
        session_backend,
        alias,
        CommittedLifecycle::Delete,
    )
    .await
}

/// Does state still require re-pointing after an agent rename was committed?
///
/// Unlike delete recovery, memory checks the alias identity row through
/// `Memory::count_agent`, exactly mirroring `Memory::rename_agent` even when
/// the old alias owns no memory entries.
pub async fn committed_rename_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
) -> bool {
    committed_residue_exists(
        config,
        mem,
        session_backend,
        alias,
        CommittedLifecycle::Rename,
    )
    .await
}

/// Durable archive location and any workspace-archive failures that must be
/// surfaced alongside the owned-store cascade.
#[derive(Debug)]
pub struct AgentDeletionArchive {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// How many leaf names one allocation may try before giving up.
///
/// Each attempt is a single `create_dir` syscall, and the names only collide
/// within one clock second, so this bound is far above the number of duplicate
/// lifecycle requests a surface can land in that window.
const ARCHIVE_LEAF_ATTEMPTS: u32 = 64;

/// Reserve a fresh archive leaf for `alias` under `root`, creating it exclusively.
///
/// The leaf name is derived from the alias and the current UTC second, so two
/// deletions of the same alias inside one second used to resolve to the same
/// directory. Because the cascade exports with truncating writes, the later
/// attempt could then overwrite an earlier non-empty export with its own
/// post-purge (empty) one and leave no recoverable copy. `create_dir` fails
/// with `AlreadyExists` instead of adopting an occupied directory, so each
/// attempt owns a distinct leaf and no export can be truncated by a duplicate.
async fn allocate_archive_dir(root: &Path, alias: &str, ts: &str) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(root).await?;
    for attempt in 0..ARCHIVE_LEAF_ATTEMPTS {
        let candidate = if attempt == 0 {
            root.join(format!("{alias}-{ts}"))
        } else {
            root.join(format!("{alias}-{ts}-{attempt}"))
        };
        match tokio::fs::create_dir(&candidate).await {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "no free archive directory for `{alias}` under {}",
            root.display()
        ),
    ))
}

/// Create the canonical agent-deletion archive and move its workspace into it.
///
/// Every lifecycle surface routes its deletion archive through here so that all
/// of them share one exclusive leaf allocator and one fail-toward-residue
/// workspace probe.
pub async fn archive_agent_workspace(
    config: &Config,
    alias: &str,
    workspace: &Path,
) -> AgentDeletionArchive {
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let archive_root = config.data_dir.join("agents").join("_deleted");
    let mut warnings = Vec::new();
    let archive_dir = match allocate_archive_dir(&archive_root, alias, &ts).await {
        Ok(dir) => dir,
        Err(error) => {
            // Report the unsuffixed leaf. Whichever way allocation failed, the
            // cascade cannot complete against it: an unusable archive root
            // fails again when the cascade creates its own subdirectory, and an
            // exhausted name space means the leaf belongs to another attempt,
            // whose exports the cascade refuses to replace. Either way the
            // purge does not run and the deletion stays retryable.
            let archive_dir = archive_root.join(format!("{alias}-{ts}"));
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "agent": alias,
                        "archive": archive_dir.display().to_string(),
                        "error": error.to_string(),
                    })),
                "agent delete: archive directory creation failed"
            );
            warnings.push(format!(
                "archive directory creation failed ({}): {error}",
                archive_dir.display()
            ));
            archive_dir
        }
    };
    match inspect_lifecycle_path(workspace).await {
        PathPresence::Present => {
            let destination = archive_dir.join("workspace");
            if let Err(error) = tokio::fs::rename(workspace, &destination).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "agent": alias,
                            "from": workspace.display().to_string(),
                            "to": destination.display().to_string(),
                            "error": error.to_string(),
                        })),
                    "agent delete: workspace archive failed"
                );
                warnings.push(format!(
                    "workspace archive failed ({} -> {}): {error}",
                    workspace.display(),
                    destination.display()
                ));
            }
        }
        PathPresence::Absent => {}
        PathPresence::Uninspectable(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "agent": alias,
                        "workspace": workspace.display().to_string(),
                        "error": error.clone(),
                    })),
                "agent delete: workspace inspection failed"
            );
            warnings.push(format!(
                "workspace inspection failed ({}): {error}",
                workspace.display()
            ));
        }
    }

    AgentDeletionArchive {
        path: archive_dir,
        warnings,
    }
}

/// What the owned-state cascade removed.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedStateReport {
    pub memory_purged: usize,
    pub knowledge_purged: usize,
    #[serde(default)]
    pub knowledge_foreign_edges_purged: usize,
    pub cron_removed: usize,
    pub acp_removed: usize,
    pub sessions_cleared: usize,
    pub control_plane_tasks_removed: usize,
    pub archived_to: Option<String>,
    /// Surfaced failures (export / purge / delete errors). Non-empty means part
    /// of the cascade did NOT complete — those rows were not silently treated as
    /// removed. The handler logs these; nothing is masked as success.
    pub warnings: Vec<String>,
}

/// Write one archive artifact, refusing to replace an existing one.
///
/// Every artifact is written exactly once per cascade, and each cascade owns a
/// freshly reserved archive leaf, so an existing file here means two attempts
/// somehow reached the same leaf. `create_new` turns that into a reported
/// failure, which keeps the purge from running, rather than letting the second
/// attempt overwrite an export the first one already made durable.
async fn write_json(path: &Path, bytes: Vec<u8>) -> anyhow::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .with_context(|| format!("open archive file {}", path.display()))?;
    file.write_all(&bytes)
        .await
        .with_context(|| format!("write archive file {}", path.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("sync archive file {}", path.display()))?;
    drop(file);

    // Persist the directory entry as well as the file contents on platforms
    // that support syncing directories. A successful return is the deletion
    // gate for knowledge rows, so it must mean the recovery path is durable.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let parent_dir = tokio::fs::File::open(parent)
            .await
            .with_context(|| format!("open archive directory {}", parent.display()))?;
        parent_dir
            .sync_all()
            .await
            .with_context(|| format!("sync archive directory {}", parent.display()))?;
    }

    Ok(())
}

#[cfg(test)]
struct KnowledgeArchivePause {
    arrived: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static KNOWLEDGE_ARCHIVE_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, KnowledgeArchivePause>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
pub(crate) fn pause_knowledge_archive_under(
    root: PathBuf,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (arrived, reached) = tokio::sync::oneshot::channel();
    let (resume, wait) = tokio::sync::oneshot::channel();
    assert!(
        KNOWLEDGE_ARCHIVE_PAUSES
            .lock()
            .unwrap()
            .insert(
                root,
                KnowledgeArchivePause {
                    arrived,
                    resume: wait
                }
            )
            .is_none()
    );
    (reached, resume)
}

#[cfg(test)]
async fn wait_at_knowledge_archive(path: &Path) {
    let pause = {
        let mut pauses = KNOWLEDGE_ARCHIVE_PAUSES.lock().unwrap();
        let key = pauses.keys().find(|root| path.starts_with(root)).cloned();
        key.and_then(|key| pauses.remove(&key))
    };
    if let Some(pause) = pause {
        pause.arrived.send(()).unwrap();
        pause.resume.await.unwrap();
    }
}

fn archive_warning(kind: &str, err: &anyhow::Error) -> String {
    format!("{kind} archive: {err}")
}

fn knowledge_purge_skipped_warning(err: &anyhow::Error) -> String {
    let error = err.to_string();
    crate::i18n::get_required_cli_string_with_args(
        "cli-alias-knowledge-purge-skipped",
        &[("error", error.as_str())],
    )
}

/// Capture retirement evidence before releasing lifecycle serialization or
/// awaiting archive work. It is immutable evidence, never a live grant.
pub fn prepare_knowledge_retirement(
    config: &Config,
    alias: &str,
) -> Result<Option<serde_json::Value>, String> {
    let path = config.knowledge.resolved_db_path();
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("knowledge graph inspection: {error}")),
        Ok(_) => {
            zeroclaw_memory::knowledge_graph::KnowledgeGraph::export_existing_owner(&path, alias)
                .map(Some)
                .map_err(|error| error.to_string())
        }
    }
}

pub async fn cascade_owned_state(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
) -> OwnedStateReport {
    let retirement = prepare_knowledge_retirement(config, alias);
    cascade_owned_state_with_retirement(
        config,
        mem,
        session_backend,
        alias,
        archive_dir,
        retirement,
    )
    .await
}

pub async fn cascade_owned_state_with_retirement(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
    retirement: Result<Option<serde_json::Value>, String>,
) -> OwnedStateReport {
    let cascade_dir = archive_dir.join("cascade");
    let mut warnings: Vec<String> = Vec::new();
    if let Err(err) = tokio::fs::create_dir_all(&cascade_dir).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": cascade_dir.display().to_string(), "err": err.to_string()})),
            "owned-state cascade: failed to create archive directory"
        );
        warnings.push(format!(
            "cascade archive directory creation failed ({}): {err}",
            cascade_dir.display()
        ));
    }

    let resolved_memory = match resolve_memory_for_owned_state(config, mem) {
        Ok(memory) => memory,
        Err(error) => {
            warnings.push(format!("memory backend unavailable: {error}"));
            None
        }
    };

    // ── memory: export → archive → purge. Failures are SURFACED in `warnings`,
    // not masked as 0 (markdown/none have no DB rows — their memory lives in the
    // archived workspace — but a real backend error must stay visible). ────────
    let memory_purged = if let Some(mem) = resolved_memory.as_ref() {
        match mem
            .export_agent(alias)
            .await
            .context("export owned memory")
            .and_then(|rows| serde_json::to_vec_pretty(&rows).context("serialize memory export"))
        {
            Ok(bytes) => match write_json(&cascade_dir.join("memory.json"), bytes).await {
                Ok(()) => match mem.purge_agent(alias).await {
                    Ok(n) => n,
                    Err(e) => {
                        warnings.push(format!("memory purge: {e}"));
                        0
                    }
                },
                Err(err) => {
                    warnings.push(archive_warning("memory", &err));
                    0
                }
            },
            Err(err) => {
                warnings.push(archive_warning("memory", &err));
                0
            }
        }
    } else {
        0
    };

    // ── knowledge graph: export → archive → purge. The graph owns durable
    // alias attribution, so it participates in the same canonical lifecycle
    // cascade as memory/session state. Avoid creating an empty DB when the
    // operator has never enabled knowledge.
    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_purge = match inspect_lifecycle_path(&knowledge_path).await {
        PathPresence::Present => match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &knowledge_path,
            config.knowledge.max_nodes,
        ) {
            Ok(graph) => match retirement {
                Ok(Some(snapshot)) => match serde_json::to_vec_pretty(&snapshot)
                    .context("serialize owned knowledge export")
                {
                    Ok(bytes) => match write_json(&cascade_dir.join("knowledge.json"), bytes).await
                    {
                        Ok(()) => {
                            #[cfg(test)]
                            wait_at_knowledge_archive(&cascade_dir).await;
                            match graph.purge_archived_owner(alias, &snapshot) {
                                Ok(report) => report,
                                Err(error) => {
                                    warnings.push(knowledge_purge_skipped_warning(&error));
                                    Default::default()
                                }
                            }
                        }
                        Err(error) => {
                            warnings.push(knowledge_purge_skipped_warning(&error));
                            Default::default()
                        }
                    },
                    Err(error) => {
                        warnings.push(knowledge_purge_skipped_warning(&error));
                        Default::default()
                    }
                },
                Ok(None) => Default::default(),
                Err(error) => {
                    warnings.push(knowledge_purge_skipped_warning(&anyhow::Error::msg(error)));
                    Default::default()
                }
            },
            Err(e) => {
                warnings.push(format!("knowledge graph open: {e}"));
                Default::default()
            }
        },
        PathPresence::Absent => Default::default(),
        PathPresence::Uninspectable(error) => {
            warnings.push(format!(
                "knowledge graph inspection ({}): {error}",
                knowledge_path.display()
            ));
            Default::default()
        }
    };

    // ── cron: list → archive → remove (cron_runs cascade off job_id) ─────────
    let cron_removed = match crate::cron::list_jobs_by_agent(config, alias) {
        Ok(jobs) => match serde_json::to_vec_pretty(&jobs).context("serialize cron export") {
            Ok(bytes) => match write_json(&cascade_dir.join("cron.json"), bytes).await {
                Ok(()) => match crate::cron::remove_jobs_by_agent(config, alias) {
                    Ok(n) => n,
                    Err(error) => {
                        warnings.push(format!("cron remove: {error}"));
                        0
                    }
                },
                Err(error) => {
                    warnings.push(archive_warning("cron", &error));
                    0
                }
            },
            Err(error) => {
                warnings.push(archive_warning("cron", &error));
                0
            }
        },
        Err(e) => {
            warnings.push(format!("cron list: {e}"));
            0
        }
    };

    // ── acp: list → archive → delete (only killed sessions remain) ───────────
    let mut acp_removed = 0;
    match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => match store.list_sessions_by_agent(alias) {
            Ok(sessions) => {
                // AcpSessionSummary isn't Serialize; hand-map the fields we keep.
                let json: Vec<serde_json::Value> = sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "session_uuid": s.session_uuid,
                            "agent_alias": s.agent_alias,
                            "workspace_dir": s.workspace_dir,
                            "token_count": s.token_count,
                            "message_count": s.message_count,
                            "created_at": s.created_at.to_rfc3339(),
                            "last_activity": s.last_activity.to_rfc3339(),
                        })
                    })
                    .collect();
                match serde_json::to_vec_pretty(&json).context("serialize ACP export") {
                    Ok(bytes) => match write_json(&cascade_dir.join("acp.json"), bytes).await {
                        Ok(()) => match store.delete_sessions_by_agent(alias) {
                            Ok(n) => acp_removed = n,
                            Err(error) => warnings.push(format!("acp delete: {error}")),
                        },
                        Err(error) => warnings.push(archive_warning("ACP", &error)),
                    },
                    Err(error) => warnings.push(archive_warning("ACP", &error)),
                }
            }
            Err(error) => warnings.push(format!("acp list: {error}")),
        },
        Err(e) => warnings.push(format!("acp store open: {e}")),
    }

    // ── session metadata: clear the stale agent attribution (keep the convo) ─
    let resolved_session_backend =
        match resolve_session_backend_for_owned_state(config, session_backend) {
            Ok(backend) => backend,
            Err(error) => {
                warnings.push(format!("session backend unavailable: {error}"));
                None
            }
        };
    let sessions_cleared = match resolved_session_backend.as_ref() {
        Some(b) => match b.clear_agent_attribution(alias) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("session attribution clear: {e}"));
                0
            }
        },
        None => 0,
    };

    let control_plane_tasks_removed = if config.data_dir.join("control_plane.db").exists() {
        let data_dir = config.data_dir.clone();
        let alias = alias.to_string();
        match tokio::task::spawn_blocking(move || {
            crate::control_plane::SqliteTaskStore::new(&data_dir)?
                .delete_by_agent(&alias)
                .map(|count| count as usize)
        })
        .await
        {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => {
                warnings.push(format!("control-plane task delete: {error}"));
                0
            }
            Err(error) => {
                warnings.push(format!("control-plane task delete task failed: {error}"));
                0
            }
        }
    } else {
        0
    };

    if !warnings.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"agent": alias, "warnings": warnings})),
            "owned-state cascade completed with warnings (some state may not have been removed)"
        );
    }

    let mut report = OwnedStateReport {
        memory_purged,
        knowledge_purged: knowledge_purge.total(),
        knowledge_foreign_edges_purged: knowledge_purge.affected_foreign_edges,
        cron_removed,
        acp_removed,
        sessions_cleared,
        control_plane_tasks_removed,
        archived_to: Some(archive_dir.display().to_string()),
        warnings,
    };

    // ── manifest: a self-describing record of the bundle ────────────────────
    let manifest = serde_json::json!({
        "alias": alias,
        "memory_rows": report.memory_purged,
        "knowledge_rows": report.knowledge_purged,
        "knowledge_foreign_edges": report.knowledge_foreign_edges_purged,
        "cron_jobs": report.cron_removed,
        "acp_sessions": report.acp_removed,
        "sessions_cleared": report.sessions_cleared,
        "control_plane_tasks": report.control_plane_tasks_removed,
        "warnings": report.warnings,
    });
    match serde_json::to_vec_pretty(&manifest).context("serialize cascade manifest") {
        Ok(bytes) => {
            if let Err(err) = write_json(&archive_dir.join("manifest.json"), bytes).await {
                report.warnings.push(archive_warning("manifest", &err));
            }
        }
        Err(err) => report.warnings.push(archive_warning("manifest", &err)),
    }

    report
}

/// What the agent-rename owned-state cascade re-pointed
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenameStateReport {
    pub memory_rows: usize,
    pub knowledge_rows: usize,
    pub cron_jobs: usize,
    pub acp_sessions: usize,
    pub sessions_repointed: usize,
    /// Surfaced failures. Non-empty means part of the cascade did NOT complete —
    /// those rows were not silently treated as re-pointed.
    pub warnings: Vec<String>,
}

pub async fn cascade_rename_agent(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    from: &str,
    to: &str,
) -> RenameStateReport {
    let mut warnings: Vec<String> = Vec::new();

    let resolved_memory = match resolve_memory_for_owned_state(config, mem) {
        Ok(memory) => memory,
        Err(error) => {
            warnings.push(format!("memory backend unavailable: {error}"));
            None
        }
    };

    let memory_rows = if let Some(mem) = resolved_memory.as_ref() {
        match mem.rename_agent(from, to).await {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("memory rename: {e}"));
                0
            }
        }
    } else {
        0
    };

    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_rows = match inspect_lifecycle_path(&knowledge_path).await {
        PathPresence::Present => match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &knowledge_path,
            config.knowledge.max_nodes,
        ) {
            Ok(graph) => match graph.rename_owner(from, to) {
                Ok(n) => n,
                Err(e) => {
                    warnings.push(format!("knowledge rename: {e}"));
                    0
                }
            },
            Err(e) => {
                warnings.push(format!("knowledge graph open: {e}"));
                0
            }
        },
        PathPresence::Absent => 0,
        PathPresence::Uninspectable(error) => {
            warnings.push(format!(
                "knowledge graph inspection ({}): {error}",
                knowledge_path.display()
            ));
            0
        }
    };

    let cron_jobs = match crate::cron::rename_jobs_by_agent(config, from, to) {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("cron rename: {e}"));
            0
        }
    };

    let acp_sessions = match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => match store.rename_sessions_by_agent(from, to) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("acp rename: {e}"));
                0
            }
        },
        Err(e) => {
            warnings.push(format!("acp store open: {e}"));
            0
        }
    };

    let resolved_session_backend =
        match resolve_session_backend_for_owned_state(config, session_backend) {
            Ok(backend) => backend,
            Err(error) => {
                warnings.push(format!("session backend unavailable: {error}"));
                None
            }
        };
    let sessions_repointed = match resolved_session_backend.as_ref() {
        Some(b) => match b.rename_agent_attribution(from, to) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("session attribution rename: {e}"));
                0
            }
        },
        None => 0,
    };

    if !warnings.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"from": from, "to": to, "warnings": warnings})),
            "rename owned-state cascade completed with warnings (some state may not have been re-pointed)"
        );
    }

    RenameStateReport {
        memory_rows,
        knowledge_rows,
        cron_jobs,
        acp_sessions,
        sessions_repointed,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lifecycle probe must answer the same way whichever shape the host
    /// reports a blocked path in. Unix returns `ENOTDIR` from `try_exists`,
    /// Windows returns an ordinary `Ok(false)`; both are driven explicitly here
    /// so the contract is proven on a single host rather than only on the
    /// platform this suite happens to run on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn knowledge_archive_barrier_keeps_unarchived_foreign_edges() {
        use zeroclaw_memory::knowledge_graph::{
            KnowledgeGraph, KnowledgeScope, NodeType, Relation,
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        let mut config = config;
        config.knowledge.db_path = tmp.path().join("graph.db").to_string_lossy().into_owned();
        let graph = KnowledgeGraph::new(&config.knowledge.resolved_db_path(), 100).unwrap();
        let retired = KnowledgeScope::for_agent("retired", Vec::new());
        let peer = KnowledgeScope::for_agent("peer", vec!["retired".into()]);
        let node = graph
            .add_node(&retired, NodeType::Pattern, "old", "keep", &[], None)
            .unwrap();
        let other = graph
            .add_node(&peer, NodeType::Expert, "peer", "keep", &[], None)
            .unwrap();
        let retirement = prepare_knowledge_retirement(&config, "retired");
        let archive = tmp.path().join("archive");
        let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        KNOWLEDGE_ARCHIVE_PAUSES.lock().unwrap().insert(
            archive.join("cascade"),
            KnowledgeArchivePause {
                arrived: arrived_tx,
                resume: resume_rx,
            },
        );
        let worker_config = config.clone();
        let worker_archive = archive.clone();
        let task = zeroclaw_spawn::spawn!(async move {
            cascade_owned_state_with_retirement(
                &worker_config,
                None,
                None,
                "retired",
                &worker_archive,
                retirement,
            )
            .await
        });
        arrived_rx.await.unwrap();
        let archived: serde_json::Value =
            serde_json::from_slice(&std::fs::read(archive.join("cascade/knowledge.json")).unwrap())
                .unwrap();
        assert!(
            archived["affected_foreign_edges"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            zeroclaw_config::alias_refs::create_map_key_checked(&mut config, "agents", "retired")
                .is_err()
        );
        let writer = KnowledgeGraph::new(&config.knowledge.resolved_db_path(), 100).unwrap();
        writer
            .add_edge(&peer, &other, &node, Relation::Uses)
            .unwrap();
        resume_tx.send(()).unwrap();
        let report = task.await.unwrap();
        assert_eq!(report.knowledge_purged, 0);
        assert_eq!(report.knowledge_foreign_edges_purged, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("changed after retirement snapshot"))
        );
        assert!(graph.get_node(&retired, &node).unwrap().is_some());
        assert_eq!(graph.find_outbound(&peer, &other, 10).unwrap().len(), 1);
        let retry =
            cascade_owned_state(&config, None, None, "retired", &tmp.path().join("retry")).await;
        assert_eq!(
            retry.knowledge_purged, 2,
            "one owned node and its foreign edge"
        );
        assert_eq!(retry.knowledge_foreign_edges_purged, 1);
    }

    #[tokio::test]
    async fn knowledge_residue_probe_under_writer_does_not_wait_for_sqlite_writer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = knowledge_lifecycle_config(&tmp);
        config.memory.backend = "none".into();
        let path = config.knowledge.resolved_db_path();
        drop(zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(&path, 100).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let authority = crate::LiveConfigAuthority::new(config.clone());
        let writer = authority.config_write_lock();
        let _guard = writer.lock().await;
        let before = std::time::Instant::now();
        assert!(committed_delete_residue_exists(&config, None, None, "retired").await);
        assert!(
            before.elapsed() < std::time::Duration::from_secs(1),
            "read-only probe must fail immediately on contention"
        );
        connection.execute_batch("ROLLBACK;").unwrap();
    }

    #[tokio::test]
    async fn knowledge_old_schema_retirement_migrates_then_retry_converges() {
        use zeroclaw_memory::knowledge_graph::{KnowledgeGraph, KnowledgeScope, NodeType};
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.knowledge.db_path = tmp.path().join("graph.db").to_string_lossy().into_owned();
        let path = config.knowledge.resolved_db_path();
        let graph = KnowledgeGraph::new(&path, 100).unwrap();
        let id = graph
            .add_node(
                &KnowledgeScope::for_agent("retired", Vec::new()),
                NodeType::Pattern,
                "retained",
                "payload",
                &[],
                None,
            )
            .unwrap();
        rusqlite::Connection::open(&path).unwrap().execute_batch("DROP TRIGGER edges_insertion_generation; ALTER TABLE edges DROP COLUMN generation;").unwrap();
        assert!(prepare_knowledge_retirement(&config, "retired").is_err());
        assert!(committed_delete_residue_exists(&config, None, None, "retired").await);
        let generation_columns: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('edges') WHERE name='generation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            generation_columns, 0,
            "residue probe must not migrate under writer"
        );
        // The cascade opens/migrates outside config locking, but cannot purge
        // from evidence that was unavailable before lifecycle serialization ended.
        let first =
            cascade_owned_state(&config, None, None, "retired", &tmp.path().join("first")).await;
        assert_eq!(first.knowledge_purged, 0);
        assert!(!first.warnings.is_empty());
        assert_eq!(graph.count_owner("retired").unwrap(), 1);
        let retry_path = tmp.path().join("retry");
        let retry = cascade_owned_state(&config, None, None, "retired", &retry_path).await;
        assert_eq!(retry.knowledge_purged, 1, "{:?}", retry.warnings);
        assert!(retry.warnings.is_empty());
        let archive: serde_json::Value = serde_json::from_slice(
            &std::fs::read(retry_path.join("cascade/knowledge.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(archive["nodes"][0]["id"], id);
        assert_eq!(graph.count_owner("retired").unwrap(), 0);
    }

    #[tokio::test]
    async fn knowledge_retirement_fences_recreation_and_preserves_new_rows() {
        use zeroclaw_memory::knowledge_graph::{KnowledgeGraph, KnowledgeScope, NodeType};
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .into_owned();
        let graph = KnowledgeGraph::new(&config.knowledge.resolved_db_path(), 100).unwrap();
        let scope = KnowledgeScope::for_agent("retired", Vec::new());
        let id = graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "retired",
                "archived bytes",
                &[],
                None,
            )
            .unwrap();
        let retirement = prepare_knowledge_retirement(&config, "retired");
        assert!(
            zeroclaw_config::alias_refs::create_map_key_checked(&mut config, "agents", "retired")
                .is_err()
        );
        let first = tmp.path().join("first");
        let report = cascade_owned_state_with_retirement(
            &config,
            None,
            None,
            "retired",
            &first,
            retirement.clone(),
        )
        .await;
        assert_eq!(report.knowledge_purged, 1, "{:?}", report.warnings);
        let archived: serde_json::Value =
            serde_json::from_slice(&std::fs::read(first.join("cascade/knowledge.json")).unwrap())
                .unwrap();
        assert_eq!(archived["nodes"][0]["id"], id);
        assert!(
            zeroclaw_config::alias_refs::create_map_key_checked(&mut config, "agents", "retired")
                .unwrap()
        );
        let new_id = graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "new incarnation",
                "must survive",
                &[],
                None,
            )
            .unwrap();
        let retry = cascade_owned_state_with_retirement(
            &config,
            None,
            None,
            "retired",
            &tmp.path().join("stale"),
            retirement,
        )
        .await;
        assert_eq!(retry.knowledge_purged, 0);
        assert!(!retry.warnings.is_empty());
        assert_eq!(
            graph.get_node(&scope, &new_id).unwrap().unwrap().content,
            "must survive"
        );
    }

    #[tokio::test]
    async fn a_blocked_path_is_uninspectable_in_both_probe_shapes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "stands where a directory must be").unwrap();
        let blocked = blocker.join("agents").join("alpha").join("workspace");

        // The Windows shape: the platform reports plain absence.
        assert!(
            classify_presence(&blocked, Ok(false))
                .await
                .is_uninspectable(),
            "a negative probe over a non-directory ancestor is not absence"
        );

        // The Unix shape: the platform reports a metadata error.
        let unix_shape = std::io::Error::from_raw_os_error(20);
        assert!(
            classify_presence(&blocked, Err(unix_shape))
                .await
                .is_uninspectable(),
            "a metadata failure is not absence either"
        );

        // And the live probe agrees with both, whichever shape this host used.
        let live = inspect_lifecycle_path(&blocked).await;
        assert!(
            live.is_uninspectable(),
            "the live probe must fail toward residue: {live:?}"
        );
        assert!(live.reason().is_some());
    }

    /// Failing toward residue must not swallow genuine absence: a missing path
    /// under a real directory is still absent in both probe shapes, so an
    /// ordinary delete or rename is not turned into a permanent retry.
    #[tokio::test]
    async fn a_missing_path_under_a_real_directory_is_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("agents").join("alpha").join("workspace");

        assert_eq!(
            classify_presence(&missing, Ok(false)).await,
            PathPresence::Absent
        );
        assert_eq!(inspect_lifecycle_path(&missing).await, PathPresence::Absent);

        let present = tmp.path().join("present");
        std::fs::create_dir_all(&present).unwrap();
        assert_eq!(
            inspect_lifecycle_path(&present).await,
            PathPresence::Present
        );
    }

    /// The residue contract is what the surfaces actually consult, so pin it to
    /// the platform-independent answer rather than to the probe shape: a
    /// workspace that cannot be inspected keeps a committed delete retryable.
    #[tokio::test]
    async fn committed_delete_residue_covers_an_uninspectable_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            config_path: tmp.path().join("install").join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .to_string();

        // Nothing owned anywhere: the alias is converged.
        std::fs::create_dir_all(config.install_root_dir()).unwrap();
        assert!(!committed_delete_residue_exists(&config, None, None, "victim").await);

        // Now block the workspace's ancestors with a file. Whether this host
        // reports that as an error or as plain absence, it is residue.
        let agents_root = config.install_root_dir().join("agents");
        std::fs::write(&agents_root, "blocks child metadata").unwrap();
        let workspace = config.agent_workspace_dir("victim");
        assert!(
            inspect_lifecycle_path(&workspace).await.is_uninspectable(),
            "fixture must block the workspace probe"
        );
        assert!(
            committed_delete_residue_exists(&config, None, None, "victim").await,
            "an uninspectable workspace is residue the retry must see"
        );
    }

    fn knowledge_lifecycle_config(tmp: &tempfile::TempDir) -> Config {
        let mut config = Config {
            config_path: tmp.path().join("install/config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;
        config.knowledge.db_path = tmp
            .path()
            .join("graph/knowledge.db")
            .to_string_lossy()
            .into();
        std::fs::create_dir_all(config.install_root_dir()).unwrap();
        config
    }

    async fn blocked_knowledge_cascade_preserves_then_recovers(rename: bool) {
        use zeroclaw_memory::knowledge_graph::{KnowledgeGraph, KnowledgeScope, NodeType};

        let tmp = tempfile::TempDir::new().unwrap();
        let config = knowledge_lifecycle_config(&tmp);
        let path = config.knowledge.resolved_db_path();
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        let old = KnowledgeScope::for_agent("retired", Vec::new());
        let peer = KnowledgeScope::for_agent("peer", Vec::new());
        let graph = KnowledgeGraph::new(&path, config.knowledge.max_nodes).unwrap();
        let node = graph
            .add_node(&old, NodeType::Pattern, "owned", "recover me", &[], None)
            .unwrap();
        let peer_node = graph
            .add_node(&peer, NodeType::Pattern, "peer", "keep me", &[], None)
            .unwrap();
        drop(graph);

        let parked = tmp.path().join("parked-graph");
        std::fs::rename(parent, &parked).unwrap();
        std::fs::write(parent, "blocked parent").unwrap();
        // Run the same cascade with both the host probe and Windows' Ok(false).
        for negative_probe in [false, true] {
            let cascade = async {
                if rename {
                    let report =
                        cascade_rename_agent(&config, None, None, "retired", "renamed").await;
                    assert_eq!(report.knowledge_rows, 0);
                    report.warnings
                } else {
                    let report = cascade_owned_state(
                        &config,
                        None,
                        None,
                        "retired",
                        &tmp.path().join(format!("blocked-{negative_probe}")),
                    )
                    .await;
                    assert_eq!(report.knowledge_purged, 0);
                    report.warnings
                }
            };
            let warnings = if negative_probe {
                NEGATIVE_PROBE_PATH.scope(path.clone(), cascade).await
            } else {
                cascade.await
            };
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("knowledge graph inspection")),
                "{warnings:?}"
            );
            assert!(committed_delete_residue_exists(&config, None, None, "retired").await);
            assert!(committed_rename_residue_exists(&config, None, None, "retired").await);
        }
        std::fs::remove_file(parent).unwrap();
        std::fs::rename(&parked, parent).unwrap();
        let graph = KnowledgeGraph::new(&path, config.knowledge.max_nodes).unwrap();
        assert!(
            graph.get_node(&old, &node).unwrap().is_some(),
            "failed cascades must preserve ownership"
        );
        drop(graph);

        if rename {
            let report = cascade_rename_agent(&config, None, None, "retired", "renamed").await;
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert_eq!(report.knowledge_rows, 1);
        } else {
            let archive = tmp.path().join("retry");
            let report = cascade_owned_state(&config, None, None, "retired", &archive).await;
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert_eq!(report.knowledge_purged, 1);
            let exported = std::fs::read_to_string(archive.join("cascade/knowledge.json")).unwrap();
            assert!(
                exported.contains(&node),
                "purge must leave a recoverable export"
            );
        }
        let graph = KnowledgeGraph::new(&path, config.knowledge.max_nodes).unwrap();
        assert!(
            graph.get_node(&old, &node).unwrap().is_none(),
            "reusing the retired alias must not recover old rows"
        );
        assert!(graph.get_node(&peer, &peer_node).unwrap().is_some());
        if rename {
            let renamed = KnowledgeScope::for_agent("renamed", Vec::new());
            assert!(graph.get_node(&renamed, &node).unwrap().is_some());
        }
        assert!(!committed_delete_residue_exists(&config, None, None, "retired").await);
        assert!(!committed_rename_residue_exists(&config, None, None, "retired").await);
    }

    #[tokio::test]
    async fn knowledge_delete_warns_preserves_and_retries_blocked_parent_in_both_probe_shapes() {
        blocked_knowledge_cascade_preserves_then_recovers(false).await;
    }

    #[tokio::test]
    async fn knowledge_rename_warns_preserves_and_retries_blocked_parent_in_both_probe_shapes() {
        blocked_knowledge_cascade_preserves_then_recovers(true).await;
    }

    #[tokio::test]
    async fn absent_knowledge_cascades_are_quiet_and_do_not_create_database() {
        for negative_probe in [false, true] {
            let tmp = tempfile::TempDir::new().unwrap();
            let config = knowledge_lifecycle_config(&tmp);
            let path = config.knowledge.resolved_db_path();
            let cascades = async {
                let deleted = cascade_owned_state(
                    &config,
                    None,
                    None,
                    "retired",
                    &tmp.path().join("archive"),
                )
                .await;
                assert_eq!(deleted.knowledge_purged, 0);
                assert!(deleted.warnings.is_empty(), "{:?}", deleted.warnings);
                let renamed = cascade_rename_agent(&config, None, None, "retired", "renamed").await;
                assert_eq!(renamed.knowledge_rows, 0);
                assert!(renamed.warnings.is_empty(), "{:?}", renamed.warnings);
                assert!(!path.exists());
                assert!(!path.parent().unwrap().exists());
            };
            if negative_probe {
                NEGATIVE_PROBE_PATH.scope(path.clone(), cascades).await;
            } else {
                cascades.await;
            }
        }
    }

    fn seed_owned_cron_job(config: &Config, alias: &str, prompt: &str) {
        crate::cron::add_agent_job(
            config,
            alias,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            prompt,
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            false,
        )
        .unwrap();
    }

    /// Two deletes of the same alias inside one clock second must not land in
    /// the same archive. The cascade exports with truncating writes, so a shared
    /// leaf lets the second attempt overwrite the first attempt's non-empty
    /// export with its own post-purge (empty) one, and both requests still
    /// report success while no recoverable copy is left.
    #[tokio::test]
    async fn duplicate_delete_cascades_cannot_share_an_archive_or_truncate_the_first_export() {
        let tmp = tempfile::TempDir::new().unwrap();
        // `config_path` belongs under the same temporary root as `data_dir`:
        // `agent_workspace_dir` derives the workspace from the install root, so
        // leaving it at its default would archive a directory from the runner's
        // home into the temporary tree and fail with a cross-device link
        // wherever those two are separate mounts.
        let mut config = Config {
            config_path: tmp.path().join("install").join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .into_owned();
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;

        // Duplicate requests admitted concurrently, before there is a workspace
        // to move: each must still reserve a leaf of its own.
        let workspace = config.agent_workspace_dir("agent_a");
        let (first_race, second_race) = tokio::join!(
            archive_agent_workspace(&config, "agent_a", &workspace),
            archive_agent_workspace(&config, "agent_a", &workspace),
        );
        assert!(first_race.warnings.is_empty(), "{:?}", first_race.warnings);
        assert!(
            second_race.warnings.is_empty(),
            "{:?}",
            second_race.warnings
        );
        assert_ne!(
            first_race.path, second_race.path,
            "concurrent duplicate deletes must not share an archive directory"
        );

        seed_owned_cron_job(&config, "agent_a", "duplicate cascade proof");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("owned.txt"), "prior incarnation").unwrap();

        let first = archive_agent_workspace(&config, "agent_a", &workspace).await;
        assert!(first.warnings.is_empty(), "{:?}", first.warnings);
        assert!(first.path.join("workspace/owned.txt").exists());
        let first_report = cascade_owned_state(&config, None, None, "agent_a", &first.path).await;
        assert!(
            first_report.warnings.is_empty(),
            "{:?}",
            first_report.warnings
        );
        assert_eq!(first_report.cron_removed, 1);
        let first_export = std::fs::read_to_string(first.path.join("cascade/cron.json")).unwrap();
        assert!(first_export.contains("duplicate cascade proof"));

        // The duplicate reaches the cascade after the purge, so its own export
        // is empty. It must write that into its own leaf.
        let second = archive_agent_workspace(&config, "agent_a", &workspace).await;
        assert!(second.warnings.is_empty(), "{:?}", second.warnings);
        assert_ne!(
            first.path, second.path,
            "a duplicate delete must not reuse the first attempt's archive directory"
        );
        let second_report = cascade_owned_state(&config, None, None, "agent_a", &second.path).await;
        assert!(
            second_report.warnings.is_empty(),
            "{:?}",
            second_report.warnings
        );
        assert_eq!(second_report.cron_removed, 0);
        let second_export = std::fs::read_to_string(second.path.join("cascade/cron.json")).unwrap();
        assert_eq!(second_export.trim(), "[]");

        let preserved = std::fs::read_to_string(first.path.join("cascade/cron.json")).unwrap();
        assert!(
            preserved.contains("duplicate cascade proof"),
            "the duplicate truncated the only durable export: {preserved}"
        );
        assert!(
            first.path.join("workspace/owned.txt").exists(),
            "the duplicate must not disturb the first attempt's archived workspace"
        );
    }

    /// A refused workspace move leaves the directory on its original mount. The
    /// cascade still runs against the owned stores, so the alias-reuse
    /// consequence has to be stated explicitly: the workspace stays where it
    /// was, the failure is reported rather than swallowed, and the committed
    /// delete still reads as residue so a retry re-enters instead of letting a
    /// recreated alias resolve to the previous incarnation's files.
    ///
    /// The move is refused here by nesting the archive root inside the
    /// workspace, which every supported platform rejects. That stands in for
    /// any refusal an operator's layout can produce, a cross-device link being
    /// the one the review environment hit.
    #[tokio::test]
    async fn a_refused_workspace_move_stays_put_and_keeps_the_delete_retryable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let workspace = install.join("agents").join("agent_a").join("workspace");
        let mut config = Config {
            config_path: install.join("config.toml"),
            data_dir: workspace.join("data"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .to_string();
        assert_eq!(config.agent_workspace_dir("agent_a"), workspace);

        std::fs::create_dir_all(&config.data_dir).unwrap();
        std::fs::write(workspace.join("retired-marker.txt"), "prior incarnation").unwrap();

        let archive = archive_agent_workspace(&config, "agent_a", &workspace).await;
        assert!(
            archive
                .warnings
                .iter()
                .any(|warning| warning.contains("workspace archive failed")),
            "a refused move must be reported: {:?}",
            archive.warnings
        );
        assert!(
            workspace.join("retired-marker.txt").exists(),
            "the workspace stays on its original mount when the move is refused"
        );

        let report = cascade_owned_state(&config, None, None, "agent_a", &archive.path).await;
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(
            committed_delete_residue_exists(&config, None, None, "agent_a").await,
            "a workspace left in place is residue: the delete must stay retryable"
        );
    }

    /// Defense in depth behind the leaf allocator. Exclusive allocation is what
    /// normally keeps two cascades apart, but the export writes must not depend
    /// on it: handed a leaf that already holds an export, a second cascade has
    /// to fail and leave the purge undone rather than replace the durable copy.
    #[tokio::test]
    async fn a_shared_archive_leaf_cannot_replace_an_existing_export() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .into_owned();
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;

        seed_owned_cron_job(&config, "agent_a", "shared leaf proof");
        let shared = tmp.path().join("shared-archive");
        std::fs::create_dir_all(&shared).unwrap();

        let first = cascade_owned_state(&config, None, None, "agent_a", &shared).await;
        assert!(first.warnings.is_empty(), "{:?}", first.warnings);
        assert_eq!(first.cron_removed, 1);

        // Same leaf, and the store is already purged, so this export is empty.
        let second = cascade_owned_state(&config, None, None, "agent_a", &shared).await;
        assert!(
            second
                .warnings
                .iter()
                .any(|warning| warning.contains("cron archive")),
            "a refused export must be reported: {:?}",
            second.warnings
        );
        let preserved = std::fs::read_to_string(shared.join("cascade/cron.json")).unwrap();
        assert!(
            preserved.contains("shared leaf proof"),
            "the second cascade replaced the only durable export: {preserved}"
        );
    }

    #[tokio::test]
    async fn archive_failure_preserves_cron_and_acp_for_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .into_owned();
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;

        seed_owned_cron_job(&config, "agent_a", "owned cron proof");
        let acp = AcpSessionStore::new(&config.data_dir).unwrap();
        acp.create_session("owned-acp-proof", "agent_a", "/workspace", None)
            .unwrap();
        acp.mark_session_killed("owned-acp-proof").unwrap();
        drop(acp);

        let blocked_archive = tmp.path().join("blocked-archive");
        std::fs::write(&blocked_archive, "not a directory").unwrap();
        let first = cascade_owned_state(&config, None, None, "agent_a", &blocked_archive).await;
        assert_eq!(first.cron_removed, 0);
        assert_eq!(first.acp_removed, 0);
        assert!(
            first
                .warnings
                .iter()
                .any(|warning| warning.contains("cron archive"))
        );
        assert!(
            first
                .warnings
                .iter()
                .any(|warning| warning.contains("ACP archive"))
        );
        assert_eq!(
            crate::cron::list_jobs_by_agent(&config, "agent_a")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            AcpSessionStore::new(&config.data_dir)
                .unwrap()
                .list_sessions_by_agent("agent_a")
                .unwrap()
                .len(),
            1
        );

        std::fs::remove_file(&blocked_archive).unwrap();
        std::fs::create_dir(&blocked_archive).unwrap();
        let retry = cascade_owned_state(&config, None, None, "agent_a", &blocked_archive).await;
        assert_eq!(retry.cron_removed, 1);
        assert_eq!(retry.acp_removed, 1);
        assert!(retry.warnings.is_empty(), "{:?}", retry.warnings);
        assert!(
            crate::cron::list_jobs_by_agent(&config, "agent_a")
                .unwrap()
                .is_empty()
        );
        assert!(
            AcpSessionStore::new(&config.data_dir)
                .unwrap()
                .list_sessions_by_agent("agent_a")
                .unwrap()
                .is_empty()
        );
    }
}
