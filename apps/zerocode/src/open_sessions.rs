//! The sidebar's open-session list, saved so the next launch reopens it.
//!
//! The daemon already keeps every transcript; what a zerocode restart loses
//! is which sessions this client had open, in what order, and which one was
//! focused. That list is written here whenever it changes and handed to the
//! panes' resume path at startup, the same path a reconnect rebuild uses.
//!
//! One file per connection target under `<config_dir>/open-sessions/`, so a
//! client that talks to two daemons keeps two lists. Only ids, aliases, order
//! and focus are stored; queued input and drafts are not.
//!
//! Each zerocode window on a daemon keeps its own list, in a numbered slot
//! held by an OS file lock. A window takes the lowest free slot, so the first
//! window gets slot 0 and a second one slot 1; closing the second and
//! reopening it while the first still runs brings back the second's sessions.
//! A session that another live window has in its list is not restored, since
//! reattaching a session live in another window lets either window cancel it.
//! Each window publishes the list it is about to open as soon as it decides,
//! before its panes start, so other windows never judge by a stale list.
//!
//! Local connections only. Over WSS the daemon admits a resume only from the
//! client identity that owns the session right now, and a relaunched window
//! has a new one, so a saved remote list could only produce failed rows.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chat::{PaneKind, ResumeEntry, SidebarSessionSummary};
use crate::client::{RpcClient, SessionEntry, Transport};
use crate::jsonrpc::JsonRpcError;

const DIR_NAME: &str = "open-sessions";
const FORMAT_VERSION: u32 = 1;
/// Windows per daemon that keep a list; a window beyond this starts empty.
const MAX_WINDOWS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SavedPane {
    Code,
    Chat,
}

impl From<PaneKind> for SavedPane {
    fn from(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Acp => Self::Code,
            PaneKind::Chat => Self::Chat,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedSession {
    pub pane: SavedPane,
    pub session_id: String,
    pub agent_alias: String,
    pub focused: bool,
}

/// What one launch saves: the sidebar rows in order, plus the pane that was
/// on screen so the next launch opens there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane: Option<SavedPane>,
    pub sessions: Vec<SavedSession>,
}

impl Snapshot {
    pub(crate) fn from_rows(rows: &[SidebarSessionSummary], active_pane: Option<PaneKind>) -> Self {
        Self {
            active_pane: active_pane.map(SavedPane::from),
            sessions: rows
                .iter()
                .map(|row| SavedSession {
                    pane: row.pane_kind.into(),
                    session_id: row.session_id.clone(),
                    agent_alias: row.agent_alias.clone(),
                    focused: row.focused,
                })
                .collect(),
        }
    }

    /// Resume entries for the Chat and Code panes, in saved order.
    pub(crate) fn resume_entries(&self, cap: usize) -> (Vec<ResumeEntry>, Vec<ResumeEntry>) {
        let mut chat = Vec::new();
        let mut code = Vec::new();
        for saved in &self.sessions {
            let list = match saved.pane {
                SavedPane::Chat => &mut chat,
                SavedPane::Code => &mut code,
            };
            if list.len() < cap {
                list.push(ResumeEntry::saved(
                    saved.session_id.clone(),
                    saved.agent_alias.clone(),
                    saved.focused,
                ));
            }
        }
        (chat, code)
    }

    /// Keep the cross-pane order independently of the panes' resume lists.
    pub(crate) fn sidebar_order(&self) -> Vec<(PaneKind, String)> {
        self.sessions
            .iter()
            .map(|saved| {
                let pane = match saved.pane {
                    SavedPane::Code => PaneKind::Acp,
                    SavedPane::Chat => PaneKind::Chat,
                };
                (pane, saved.session_id.clone())
            })
            .collect()
    }

    /// The pane to open on: the saved one if it has sessions, else Code, else
    /// Chat, else none (stay on the dashboard).
    pub(crate) fn start_pane(&self) -> Option<PaneKind> {
        let has = |pane| self.sessions.iter().any(|s| s.pane == pane);
        match self.active_pane {
            Some(SavedPane::Code) if has(SavedPane::Code) => Some(PaneKind::Acp),
            Some(SavedPane::Chat) if has(SavedPane::Chat) => Some(PaneKind::Chat),
            _ if has(SavedPane::Code) => Some(PaneKind::Acp),
            _ if has(SavedPane::Chat) => Some(PaneKind::Chat),
            _ => None,
        }
    }
}

impl Snapshot {
    /// Drop Code sessions absent from the ACP list. If the list could not be
    /// fetched, keep them for the resume path. Chat needs a transcript probe
    /// instead of a listing. Returns how many Code sessions were dropped.
    pub(crate) fn retain_known(&mut self, code: Option<&[SessionEntry]>) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|saved| {
            saved.pane == SavedPane::Chat
                || code.is_none_or(|list| {
                    list.iter().any(|entry| {
                        entry.session_id == saved.session_id
                            && entry
                                .agent_alias
                                .as_deref()
                                .is_none_or(|alias| alias == saved.agent_alias)
                    })
                })
        });
        before - self.sessions.len()
    }
}

/// Prune Code sessions using the ACP list and Chat sessions using stored
/// history. Chat listing is not an existence check: JSONL sessions lack the
/// alias/channel metadata required by `session/list`. Keep entries on probe
/// errors except the daemon's specific missing-or-unowned session refusal.
async fn prune_missing(rpc: &RpcClient, mut saved: Snapshot) -> (Snapshot, usize) {
    let wants = |pane| saved.sessions.iter().any(|s| s.pane == pane);
    let code = if wants(SavedPane::Code) {
        rpc.acp_session_list().await.ok().map(|list| list.sessions)
    } else {
        None
    };
    let mut dropped = saved.retain_known(code.as_deref());
    let mut kept = Vec::with_capacity(saved.sessions.len());
    for session in saved.sessions {
        let missing = if session.pane == SavedPane::Chat {
            match rpc
                .session_messages_page(&session.session_id, Some(1), None)
                .await
            {
                Ok(page) => page.total == 0,
                Err(error) => error.downcast_ref::<JsonRpcError>().is_some_and(|error| {
                    error.code == -32012
                        && error
                            .message
                            .contains("Session not found or not owned by this principal")
                }),
            }
        } else {
            false
        };
        if missing {
            dropped += 1;
        } else {
            kept.push(session);
        }
    }
    saved.sessions = kept;
    (saved, dropped)
}

#[derive(Debug, Serialize, Deserialize)]
struct FileBody {
    version: u32,
    /// The connection label, for a human reading the file; the file name is
    /// its hash.
    target: String,
    #[serde(flatten)]
    snapshot: Snapshot,
}

/// Where one connection target's list lives, and the last list written there.
#[derive(Debug)]
pub(crate) struct Store {
    dir: PathBuf,
    base: String,
    slot: usize,
    target: String,
    last_saved: Option<Snapshot>,
    /// Held for the life of the store; the OS releases it when the process
    /// exits, including on a crash.
    _lock: std::fs::File,
}

/// Slot 0 keeps the unnumbered names, so the first window's list is the
/// same file a single-window user has always had.
fn slot_file(dir: &Path, base: &str, slot: usize, ext: &str) -> PathBuf {
    if slot == 0 {
        dir.join(format!("{base}.{ext}"))
    } else {
        dir.join(format!("{base}-{slot}.{ext}"))
    }
}

fn open_lock(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Whether some other process holds `path`'s lock. Probing takes the lock
/// for an instant when it is free, then drops it.
fn held_elsewhere(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(file) = open_lock(path) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// What a launch reopens, and why any saved sessions were left out.
#[derive(Debug, Default)]
pub(crate) struct Restore {
    pub snapshot: Option<Snapshot>,
    /// Open in another running window.
    pub elsewhere: usize,
    /// Code sessions absent from the ACP list, plus Chat sessions with no
    /// stored history or the daemon's missing-or-unowned session refusal.
    pub dropped: usize,
    /// The early write of this window's list failed. The restore still goes
    /// ahead, and the file keeps its previous contents; the next change to
    /// the sidebar writes again.
    pub publish_error: Option<anyhow::Error>,
}

impl Store {
    /// The store for this launch, or `Ok(None)` when the connection is not
    /// local (see the module docs) or every window slot is taken.
    pub(crate) fn for_launch(
        config_dir: &Path,
        target_label: &str,
        transport: Transport,
    ) -> Result<Option<Self>> {
        if transport != Transport::Local {
            return Ok(None);
        }
        Self::acquire(config_dir, target_label)
    }

    /// Decide what this window reopens and publish it as the window's list
    /// before any pane starts. With `restore` off (`--fresh`) the published
    /// list is empty, so the previous occupant's sessions are not claimed.
    pub(crate) async fn start(&mut self, rpc: &RpcClient, restore: bool) -> Restore {
        let mut outcome = Restore::default();
        if restore && let Some(mut saved) = self.load() {
            outcome.elsewhere = saved.without(&self.ids_open_in_other_windows());
            let (saved, dropped) = prune_missing(rpc, saved).await;
            outcome.dropped = dropped;
            outcome.snapshot = Some(saved);
        }
        let claim = outcome.snapshot.clone().unwrap_or_default();
        outcome.publish_error = self.save_if_changed(&claim).err();
        outcome
    }

    /// Take the lowest free window slot for this connection target. `Ok(None)`
    /// means every slot is held by a running window.
    fn acquire(config_dir: &Path, target_label: &str) -> Result<Option<Self>> {
        let digest = Sha256::digest(target_label.as_bytes());
        let base: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let dir = config_dir.join(DIR_NAME);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        for slot in 0..MAX_WINDOWS {
            let lock_path = slot_file(&dir, &base, slot, "lock");
            let lock = open_lock(&lock_path)?;
            match lock.try_lock() {
                Ok(()) => {
                    return Ok(Some(Self {
                        dir,
                        base,
                        slot,
                        target: target_label.to_string(),
                        last_saved: None,
                        _lock: lock,
                    }));
                }
                Err(std::fs::TryLockError::WouldBlock) => continue,
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(error).with_context(|| format!("locking {}", lock_path.display()));
                }
            }
        }
        Ok(None)
    }

    fn path(&self) -> PathBuf {
        slot_file(&self.dir, &self.base, self.slot, "json")
    }

    /// Session ids saved by the other windows that are running now. Their
    /// lists are what those windows have open, so this window must not
    /// reattach any of them.
    pub(crate) fn ids_open_in_other_windows(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        for slot in (0..MAX_WINDOWS).filter(|slot| *slot != self.slot) {
            if !held_elsewhere(&slot_file(&self.dir, &self.base, slot, "lock")) {
                continue;
            }
            if let Some(snapshot) = read_snapshot(&slot_file(&self.dir, &self.base, slot, "json")) {
                ids.extend(snapshot.sessions.into_iter().map(|s| s.session_id));
            }
        }
        ids
    }

    /// The saved list, or `None` when there is none or it cannot be read. A
    /// damaged or newer-format file is ignored, not an error: the launch goes
    /// on with an empty sidebar and the next save replaces the file.
    pub(crate) fn load(&mut self) -> Option<Snapshot> {
        let snapshot = read_snapshot(&self.path())?;
        self.last_saved = Some(snapshot.clone());
        Some(snapshot)
    }

    /// Write `snapshot` if it differs from the last one written. Returns
    /// whether a write happened. A failed write is not retried until the list
    /// changes again, so a read-only directory costs one error, not one per
    /// frame.
    pub(crate) fn save_if_changed(&mut self, snapshot: &Snapshot) -> Result<bool> {
        if self.last_saved.as_ref() == Some(snapshot) {
            return Ok(false);
        }
        self.last_saved = Some(snapshot.clone());
        let body = FileBody {
            version: FORMAT_VERSION,
            target: self.target.clone(),
            snapshot: snapshot.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&body).context("serializing open sessions")?;
        crate::secure_file::write_private_atomic(&self.path(), &bytes)?;
        Ok(true)
    }
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    let body: FileBody = serde_json::from_slice(&bytes).ok()?;
    (body.version == FORMAT_VERSION).then_some(body.snapshot)
}

impl Snapshot {
    /// Drop the sessions another running window has open. Returns how many.
    pub(crate) fn without(&mut self, ids: &std::collections::HashSet<String>) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|s| !ids.contains(&s.session_id));
        before - self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::SidebarStatus;

    fn owned(dir: &Path, target: &str) -> Store {
        Store::acquire(dir, target)
            .expect("lock file opens")
            .expect("no other window owns this list")
    }

    fn saved_ids(store: &mut Store) -> Vec<String> {
        store
            .load()
            .map(|s| s.sessions.into_iter().map(|s| s.session_id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_reopened_second_window_gets_its_own_sessions_back() {
        let dir = tempfile::tempdir().unwrap();
        let t = "local:/run/a.sock";
        let mut first = owned(dir.path(), t);
        first
            .save_if_changed(&Snapshot::from_rows(
                &[row(PaneKind::Acp, "w1", "a", true)],
                None,
            ))
            .unwrap();
        let mut second = owned(dir.path(), t);
        assert!(
            saved_ids(&mut second).is_empty(),
            "a new second window starts empty"
        );
        second
            .save_if_changed(&Snapshot::from_rows(
                &[row(PaneKind::Acp, "w2", "a", true)],
                None,
            ))
            .unwrap();

        // Close the second window and open it again while the first runs.
        drop(second);
        let mut reopened = owned(dir.path(), t);
        assert_eq!(saved_ids(&mut reopened), ["w2"]);

        // With both closed, the next launch is the first window again.
        drop(reopened);
        drop(first);
        assert_eq!(saved_ids(&mut owned(dir.path(), t)), ["w1"]);
    }

    #[test]
    fn sessions_open_in_another_running_window_are_not_restored() {
        let dir = tempfile::tempdir().unwrap();
        let t = "local:/run/a.sock";
        let mut first = owned(dir.path(), t);
        first
            .save_if_changed(&Snapshot::from_rows(
                &[row(PaneKind::Acp, "shared", "a", true)],
                None,
            ))
            .unwrap();
        let mut second = owned(dir.path(), t);
        second
            .save_if_changed(&Snapshot::from_rows(
                &[
                    row(PaneKind::Acp, "shared", "a", false),
                    row(PaneKind::Acp, "mine", "a", true),
                ],
                None,
            ))
            .unwrap();
        drop(second);

        let mut reopened = owned(dir.path(), t);
        let mut saved = reopened.load().unwrap();
        assert_eq!(saved.without(&reopened.ids_open_in_other_windows()), 1);
        let ids: Vec<_> = saved
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["mine"]);

        // A closed window's list does not block anything.
        drop(first);
        assert!(reopened.ids_open_in_other_windows().is_empty());
    }

    #[test]
    fn windows_on_another_daemon_do_not_take_slots() {
        let dir = tempfile::tempdir().unwrap();
        let _a = owned(dir.path(), "local:/run/a.sock");
        let b = owned(dir.path(), "local:/run/b.sock");
        assert_eq!(b.slot, 0);
    }

    fn row(pane: PaneKind, id: &str, alias: &str, focused: bool) -> SidebarSessionSummary {
        SidebarSessionSummary {
            session_id: id.to_string(),
            agent_alias: alias.to_string(),
            message_count: 3,
            status: SidebarStatus::Ready,
            pane_kind: pane,
            focused,
        }
    }

    #[test]
    fn saved_list_round_trips_in_order_with_focus() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row(PaneKind::Acp, "code-1", "fable", false),
            row(PaneKind::Chat, "chat-1", "fable", true),
            row(PaneKind::Acp, "code-2", "ox", true),
        ];
        let snapshot = Snapshot::from_rows(&rows, Some(PaneKind::Acp));
        let mut store = owned(dir.path(), "local:/run/a.sock");
        assert!(store.save_if_changed(&snapshot).unwrap());
        drop(store);

        let mut reopened = owned(dir.path(), "local:/run/a.sock");
        let loaded = reopened.load().expect("saved list loads");
        assert_eq!(loaded, snapshot);

        let (chat, code) = loaded.resume_entries(crate::chat::MAX_TRACKED_SESSIONS_PER_PANE);
        let ids = |entries: &[ResumeEntry]| {
            entries
                .iter()
                .map(|e| (e.session_id.clone(), e.agent_alias.clone(), e.was_focused))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&code),
            vec![
                ("code-1".into(), "fable".into(), false),
                ("code-2".into(), "ox".into(), true),
            ]
        );
        assert_eq!(ids(&chat), vec![("chat-1".into(), "fable".into(), true)]);
        assert_eq!(loaded.start_pane(), Some(PaneKind::Acp));

        // Pane reconstruction groups Code and Chat separately and may put
        // the focused session first. The saved order must win before draw.
        let mut restored_rows: Vec<_> = code
            .iter()
            .rev()
            .map(|e| row(PaneKind::Acp, &e.session_id, &e.agent_alias, e.was_focused))
            .chain(
                chat.iter()
                    .map(|e| row(PaneKind::Chat, &e.session_id, &e.agent_alias, e.was_focused)),
            )
            .collect();
        let mut sidebar = crate::agent_sidebar::AgentSidebar::new();
        sidebar.restore_order(loaded.sidebar_order());
        sidebar.sort_in_display_order(&mut restored_rows);
        assert_eq!(
            Snapshot::from_rows(&restored_rows, loaded.start_pane()),
            snapshot,
            "the cross-pane order and focus survive a save and restore"
        );
    }

    #[test]
    fn each_connection_target_keeps_its_own_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut main = owned(dir.path(), "local:/run/main.sock");
        let mut other = owned(dir.path(), "local:/run/experiment.sock");
        assert_ne!(main.path(), other.path());
        main.save_if_changed(&Snapshot::from_rows(
            &[row(PaneKind::Acp, "main-1", "fable", true)],
            None,
        ))
        .unwrap();
        assert!(
            other.load().is_none(),
            "another daemon's list must not load"
        );
        assert_eq!(main.load().unwrap().sessions[0].session_id, "main-1");
    }

    #[test]
    fn unchanged_list_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = owned(dir.path(), "local:/run/a.sock");
        let snapshot = Snapshot::from_rows(&[row(PaneKind::Chat, "c", "a", true)], None);
        assert!(store.save_if_changed(&snapshot).unwrap());
        assert!(!store.save_if_changed(&snapshot).unwrap());
        let moved = Snapshot::from_rows(&[row(PaneKind::Chat, "c", "a", false)], None);
        assert!(
            store.save_if_changed(&moved).unwrap(),
            "a focus change is saved"
        );
    }

    #[test]
    fn a_loaded_list_is_not_written_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = Snapshot::from_rows(&[row(PaneKind::Acp, "s", "a", true)], None);
        owned(dir.path(), "t").save_if_changed(&snapshot).unwrap();
        let mut store = owned(dir.path(), "t");
        let loaded = store.load().unwrap();
        assert!(!store.save_if_changed(&loaded).unwrap());
    }

    #[test]
    fn damaged_or_newer_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = owned(dir.path(), "t");
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), b"{not json").unwrap();
        assert!(store.load().is_none());
        std::fs::write(
            store.path(),
            br#"{"version":99,"target":"t","sessions":[]}"#,
        )
        .unwrap();
        assert!(store.load().is_none());
    }

    #[test]
    fn restore_limits_each_pane_to_the_supplied_cap() {
        for cap in [1, 4, 8, 32] {
            let rows: Vec<_> = (0..=cap)
                .flat_map(|i| {
                    [
                        row(PaneKind::Acp, &format!("code-{i}"), "a", i == 0),
                        row(PaneKind::Chat, &format!("chat-{i}"), "a", i == 0),
                    ]
                })
                .collect();
            let (chat, code) = Snapshot::from_rows(&rows, None).resume_entries(cap);
            for (entries, prefix) in [(chat, "chat"), (code, "code")] {
                let ids: Vec<_> = entries.into_iter().map(|e| e.session_id).collect();
                let expected: Vec<_> = (0..cap).map(|i| format!("{prefix}-{i}")).collect();
                assert_eq!(ids, expected, "cap {cap}, pane {prefix}");
            }
        }
    }

    #[test]
    fn start_pane_falls_back_to_a_pane_that_has_sessions() {
        let only_chat =
            Snapshot::from_rows(&[row(PaneKind::Chat, "c", "a", true)], Some(PaneKind::Acp));
        assert_eq!(only_chat.start_pane(), Some(PaneKind::Chat));
        assert_eq!(Snapshot::default().start_pane(), None);
    }

    fn listed(id: &str, alias: Option<&str>) -> SessionEntry {
        SessionEntry {
            session_id: id.to_string(),
            session_key: id.to_string(),
            created_at: String::new(),
            last_activity: String::new(),
            message_count: 0,
            agent_alias: alias.map(str::to_string),
            channel_id: None,
            name: None,
        }
    }

    #[test]
    fn sessions_the_daemon_no_longer_has_are_dropped() {
        let mut snapshot = Snapshot::from_rows(
            &[
                row(PaneKind::Acp, "kept", "fable", false),
                row(PaneKind::Acp, "pruned", "fable", true),
                row(PaneKind::Acp, "reassigned", "fable", false),
                row(PaneKind::Chat, "chat-kept", "ox", true),
            ],
            None,
        );
        let code = [
            listed("kept", Some("fable")),
            listed("reassigned", Some("ox")),
        ];
        assert_eq!(snapshot.retain_known(Some(&code)), 2);
        let ids: Vec<_> = snapshot
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["kept", "chat-kept"]);
    }

    #[test]
    fn an_unreadable_acp_list_keeps_code_and_leaves_chat_for_probing() {
        let mut snapshot = Snapshot::from_rows(
            &[
                row(PaneKind::Acp, "code", "a", true),
                row(PaneKind::Chat, "chat", "a", true),
            ],
            None,
        );
        let saved = snapshot.clone();
        assert_eq!(snapshot.retain_known(None), 0);
        assert_eq!(snapshot, saved);
    }

    fn rpc_on(
        transport: Transport,
    ) -> (
        std::sync::Arc<crate::jsonrpc::RpcOutbound>,
        RpcClient,
        tokio::sync::mpsc::Receiver<String>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(16);
        let out = std::sync::Arc::new(crate::jsonrpc::RpcOutbound::new(tx));
        let client = RpcClient::with_rpc_transport(std::sync::Arc::clone(&out), transport);
        (out, client, rx)
    }

    #[tokio::test]
    async fn local_restore_asks_the_daemon_and_drops_what_it_lacks() {
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let saved = Snapshot::from_rows(
            &[
                row(PaneKind::Acp, "alive", "fable", true),
                row(PaneKind::Acp, "gone", "fable", false),
            ],
            Some(PaneKind::Acp),
        );
        let task = tokio::spawn(async move { prune_missing(&client, saved).await });
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("restore should list Code sessions")
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], crate::client::method::SESSION_LIST_ACP);
        out.dispatch_response(
            request["id"].as_str().unwrap(),
            Some(serde_json::json!({ "sessions": [{
                "session_id": "alive", "session_key": "alive",
                "created_at": "", "last_activity": "", "message_count": 2,
                "agent_alias": "fable"
            }]})),
            None,
        );
        let (pruned, dropped) = task.await.unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(pruned.sessions.len(), 1);
        assert_eq!(pruned.sessions[0].session_id, "alive");
        assert!(
            rx.try_recv().is_err(),
            "no Chat history is probed without Chat rows"
        );
    }

    #[tokio::test]
    async fn deleted_chat_session_is_skipped_at_restore() {
        let dir = tempfile::tempdir().unwrap();
        let target = "local:/run/chat-restore.sock";
        let saved = Snapshot::from_rows(
            &[row(PaneKind::Chat, "deleted-chat", "alpha", true)],
            Some(PaneKind::Chat),
        );
        owned(dir.path(), target).save_if_changed(&saved).unwrap();
        let mut window = owned(dir.path(), target);
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let mut startup = tokio::spawn(async move {
            let outcome = window.start(&client, true).await;
            (window, outcome)
        });
        let (mut window, outcome) = tokio::select! {
            result = &mut startup => result.unwrap(),
            Some(line) = rx.recv() => {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], crate::client::method::SESSION_MESSAGES);
                assert_eq!(request["params"]["session_id"], "deleted-chat");
                assert_eq!(request["params"]["limit"], 1);
                assert!(request["params"].get("before_index").is_none());
                out.dispatch_response(
                    request["id"].as_str().unwrap(),
                    Some(serde_json::json!({ "messages": [], "total": 0, "start": 0 })),
                    None,
                );
                startup.await.unwrap()
            }
        };
        assert_eq!(outcome.dropped, 1);
        assert!(outcome.snapshot.unwrap().sessions.is_empty());
        assert!(window.load().unwrap().sessions.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn chat_sessions_survive_startup_without_a_complete_daemon_listing() {
        let dir = tempfile::tempdir().unwrap();
        let target = "local:/run/chat-restore.sock";
        let saved = Snapshot::from_rows(
            &[
                row(PaneKind::Chat, "jsonl-chat", "alpha", true),
                row(PaneKind::Chat, "older-chat", "alpha", false),
            ],
            Some(PaneKind::Chat),
        );
        owned(dir.path(), target).save_if_changed(&saved).unwrap();
        let mut window = owned(dir.path(), target);
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let startup = tokio::spawn(async move {
            let outcome = window.start(&client, true).await;
            (window, outcome)
        });
        for _ in 0..2 {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("restore should probe Chat history")
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            // JSONL sessions lack alias/channel metadata, so session/list can
            // be empty even when their transcripts exist. Never use it here.
            assert_eq!(request["method"], crate::client::method::SESSION_MESSAGES);
            assert_eq!(request["params"]["limit"], 1);
            assert!(request["params"].get("before_index").is_none());
            let id = request["params"]["session_id"].as_str().unwrap();
            assert!(matches!(id, "jsonl-chat" | "older-chat"));
            out.dispatch_response(
                request["id"].as_str().unwrap(),
                Some(serde_json::json!({
                    "messages": [{ "role": "user", "content": "stored history" }],
                    "total": 3, "start": 2
                })),
                None,
            );
        }
        let (mut window, outcome) = startup.await.unwrap();
        assert_eq!(outcome.dropped, 0);
        assert_eq!(outcome.snapshot.as_ref(), Some(&saved));
        assert_eq!(window.load(), Some(saved));
        let (chat, code) = outcome
            .snapshot
            .unwrap()
            .resume_entries(crate::chat::MAX_TRACKED_SESSIONS_PER_PANE);
        let ids: Vec<_> = chat.iter().map(|entry| entry.session_id.as_str()).collect();
        assert_eq!(ids, ["jsonl-chat", "older-chat"]);
        assert!(code.is_empty());
        assert!(rx.try_recv().is_err(), "Chat restore never fetches a list");
    }

    #[tokio::test]
    async fn chat_restore_drops_only_the_specific_missing_session_refusal() {
        use crate::jsonrpc::error_codes;

        for (code, message, expected_dropped) in [
            (
                -32012,
                "Session not found or not owned by this principal",
                1,
            ),
            (
                error_codes::INTERNAL_ERROR,
                "Session not found or not owned by this principal",
                0,
            ),
            (-32012, "Session environment differs", 0),
            (
                error_codes::INTERNAL_ERROR,
                "Session persistence disabled",
                0,
            ),
            (error_codes::SESSION_BUSY, "Session is busy", 0),
        ] {
            let (out, client, mut rx) = rpc_on(Transport::Local);
            let saved = Snapshot::from_rows(
                &[row(PaneKind::Chat, "chat", "alpha", true)],
                Some(PaneKind::Chat),
            );
            let original = saved.clone();
            let task = tokio::spawn(async move { prune_missing(&client, saved).await });
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("restore should probe Chat history")
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], crate::client::method::SESSION_MESSAGES);
            out.dispatch_response(
                request["id"].as_str().unwrap(),
                None,
                Some(JsonRpcError {
                    code,
                    message: message.to_string(),
                    data: None,
                }),
            );
            let (restored, dropped) = task.await.unwrap();
            assert_eq!(dropped, expected_dropped, "{code}: {message}");
            if expected_dropped == 0 {
                assert_eq!(restored, original, "{code}: {message}");
            } else {
                assert!(restored.sessions.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn mixed_restore_prunes_code_by_list_and_chat_by_history_in_order() {
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let saved = Snapshot::from_rows(
            &[
                row(PaneKind::Chat, "chat-first", "alpha", false),
                row(PaneKind::Acp, "code-gone", "alpha", false),
                row(PaneKind::Acp, "code-kept", "alpha", true),
                row(PaneKind::Chat, "chat-gone", "alpha", false),
                row(PaneKind::Chat, "chat-last", "alpha", true),
            ],
            Some(PaneKind::Acp),
        );
        let task = tokio::spawn(async move { prune_missing(&client, saved).await });
        for _ in 0..4 {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("restore should list Code or probe Chat")
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                crate::client::method::SESSION_LIST_ACP => serde_json::json!({
                    "sessions": [{
                        "session_id": "code-kept", "session_key": "code-kept",
                        "created_at": "", "last_activity": "", "message_count": 2,
                        "agent_alias": "alpha"
                    }]
                }),
                crate::client::method::SESSION_MESSAGES => {
                    assert_eq!(request["params"]["limit"], 1);
                    assert!(request["params"].get("before_index").is_none());
                    match request["params"]["session_id"].as_str().unwrap() {
                        "chat-gone" => serde_json::json!({
                            "messages": [], "total": 0, "start": 0
                        }),
                        "chat-first" | "chat-last" => serde_json::json!({
                            "messages": [{ "role": "user", "content": "stored history" }],
                            "total": 2, "start": 1
                        }),
                        id => panic!("unexpected Chat probe: {id}"),
                    }
                }
                method => panic!("unexpected restore request: {method}"),
            };
            out.dispatch_response(request["id"].as_str().unwrap(), Some(result), None);
        }
        let (restored, dropped) = task.await.unwrap();
        assert_eq!(dropped, 2);
        let ids: Vec<_> = restored
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["chat-first", "code-kept", "chat-last"]);
        assert_eq!(restored.start_pane(), Some(PaneKind::Acp));
        assert!(restored.sessions[1].focused);
        assert!(restored.sessions[2].focused);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn remote_connections_neither_save_nor_restore() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Store::for_launch(dir.path(), "wss://host:9781", Transport::Wss)
                .unwrap()
                .is_none(),
            "a relaunched remote window cannot reclaim its sessions, so it keeps no list"
        );
        assert!(
            Store::for_launch(dir.path(), "local:/run/a.sock", Transport::Local)
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_fresh_window_publishes_an_empty_list_before_its_panes_start() {
        let dir = tempfile::tempdir().unwrap();
        let t = "local:/run/a.sock";
        let mut previous = owned(dir.path(), t);
        previous
            .save_if_changed(&Snapshot::from_rows(
                &[row(PaneKind::Acp, "old", "a", true)],
                None,
            ))
            .unwrap();
        drop(previous);

        // A `--fresh` window takes the slot whose file still lists "old".
        let mut fresh = owned(dir.path(), t);
        let (_out, client, _rx) = rpc_on(Transport::Local);
        let outcome = fresh.start(&client, false).await;
        assert!(outcome.snapshot.is_none());

        // A second window must not treat "old" as open in the fresh one.
        let other = owned(dir.path(), t);
        assert!(other.ids_open_in_other_windows().is_empty());
    }

    #[tokio::test]
    async fn a_restoring_window_publishes_only_what_it_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let t = "local:/run/a.sock";
        let mut previous = owned(dir.path(), t);
        previous
            .save_if_changed(&Snapshot::from_rows(
                &[
                    row(PaneKind::Acp, "alive", "a", true),
                    row(PaneKind::Acp, "gone", "a", false),
                ],
                None,
            ))
            .unwrap();
        drop(previous);

        let mut window = owned(dir.path(), t);
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let answer = tokio::spawn(async move {
            let line = rx.recv().await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            out.dispatch_response(
                request["id"].as_str().unwrap(),
                Some(serde_json::json!({ "sessions": [{
                    "session_id": "alive", "session_key": "alive",
                    "created_at": "", "last_activity": "", "message_count": 1,
                    "agent_alias": "a"
                }]})),
                None,
            );
        });
        let outcome = window.start(&client, true).await;
        answer.await.unwrap();
        assert_eq!(outcome.dropped, 1);

        let other = owned(dir.path(), t);
        let claimed = other.ids_open_in_other_windows();
        assert!(claimed.contains("alive"));
        assert!(
            !claimed.contains("gone"),
            "a session not reopened is not claimed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_early_write_still_restores_the_list() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let t = "local:/run/a.sock";
        let mut previous = owned(dir.path(), t);
        previous
            .save_if_changed(&Snapshot::from_rows(
                &[
                    row(PaneKind::Acp, "keep", "a", true),
                    row(PaneKind::Acp, "gone", "a", false),
                ],
                None,
            ))
            .unwrap();
        drop(previous);

        let mut window = owned(dir.path(), t);
        let slots = dir.path().join(DIR_NAME);
        std::fs::set_permissions(&slots, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (out, client, mut rx) = rpc_on(Transport::Local);
        let answer = tokio::spawn(async move {
            let line = rx.recv().await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            out.dispatch_response(
                request["id"].as_str().unwrap(),
                Some(serde_json::json!({ "sessions": [{
                    "session_id": "keep", "session_key": "keep",
                    "created_at": "", "last_activity": "", "message_count": 1,
                    "agent_alias": "a"
                }]})),
                None,
            );
        });
        let outcome = window.start(&client, true).await;
        answer.await.unwrap();
        std::fs::set_permissions(&slots, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(outcome.publish_error.is_some(), "the write was refused");
        let ids: Vec<_> = outcome
            .snapshot
            .expect("the restore goes ahead")
            .sessions
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(ids, ["keep"]);
    }

    #[cfg(unix)]
    #[test]
    fn saved_list_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = owned(dir.path(), "t");
        store
            .save_if_changed(&Snapshot::from_rows(
                &[row(PaneKind::Acp, "s", "a", true)],
                None,
            ))
            .unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
