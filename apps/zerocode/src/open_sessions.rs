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

use crate::chat::{MAX_TRACKED_SESSIONS_PER_PANE, PaneKind, ResumeEntry, SidebarSessionSummary};
use crate::client::{RpcClient, SessionEntry, Transport};

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
    pub(crate) fn resume_entries(&self) -> (Vec<ResumeEntry>, Vec<ResumeEntry>) {
        let mut chat = Vec::new();
        let mut code = Vec::new();
        for saved in &self.sessions {
            let list = match saved.pane {
                SavedPane::Chat => &mut chat,
                SavedPane::Code => &mut code,
            };
            if list.len() < MAX_TRACKED_SESSIONS_PER_PANE {
                list.push(ResumeEntry::saved(
                    saved.session_id.clone(),
                    saved.agent_alias.clone(),
                    saved.focused,
                ));
            }
        }
        (chat, code)
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
    /// Drop saved sessions the daemon no longer has. `None` for a pane means
    /// its list could not be fetched: keep that pane's entries and let the
    /// resume path retry them. Returns how many were dropped.
    pub(crate) fn retain_known(
        &mut self,
        code: Option<&[SessionEntry]>,
        chat: Option<&[SessionEntry]>,
    ) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|saved| {
            let known = match saved.pane {
                SavedPane::Code => code,
                SavedPane::Chat => chat,
            };
            known.is_none_or(|list| {
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

/// Check a loaded list against the daemon before resuming it, so a session
/// deleted or pruned since the last launch is skipped instead of showing as a
/// failed row that cannot be closed.
async fn prune_missing(rpc: &RpcClient, mut saved: Snapshot) -> (Snapshot, usize) {
    let wants = |pane| saved.sessions.iter().any(|s| s.pane == pane);
    let code = if wants(SavedPane::Code) {
        rpc.acp_session_list().await.ok().map(|list| list.sessions)
    } else {
        None
    };
    let chat = if wants(SavedPane::Chat) {
        rpc.session_list(None).await.ok().map(|list| list.sessions)
    } else {
        None
    };
    let dropped = saved.retain_known(code.as_deref(), chat.as_deref());
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
    /// No longer known to the daemon.
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
            row(PaneKind::Acp, "code-2", "ox", true),
            row(PaneKind::Chat, "chat-1", "fable", true),
        ];
        let snapshot = Snapshot::from_rows(&rows, Some(PaneKind::Acp));
        let mut store = owned(dir.path(), "local:/run/a.sock");
        assert!(store.save_if_changed(&snapshot).unwrap());
        drop(store);

        let mut reopened = owned(dir.path(), "local:/run/a.sock");
        let loaded = reopened.load().expect("saved list loads");
        assert_eq!(loaded, snapshot);

        let (chat, code) = loaded.resume_entries();
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
    fn a_hand_edited_file_cannot_exceed_the_pane_cap() {
        let rows: Vec<_> = (0..12)
            .map(|i| row(PaneKind::Acp, &format!("s{i}"), "a", i == 0))
            .collect();
        let (_, code) = Snapshot::from_rows(&rows, None).resume_entries();
        assert_eq!(code.len(), MAX_TRACKED_SESSIONS_PER_PANE);
        assert_eq!(code[0].session_id, "s0");
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
        let chat = [listed("chat-kept", None)];
        assert_eq!(snapshot.retain_known(Some(&code), Some(&chat)), 2);
        let ids: Vec<_> = snapshot
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["kept", "chat-kept"]);
    }

    #[test]
    fn an_unreadable_daemon_list_keeps_that_panes_sessions() {
        let mut snapshot = Snapshot::from_rows(
            &[
                row(PaneKind::Acp, "code", "a", true),
                row(PaneKind::Chat, "chat", "a", true),
            ],
            None,
        );
        assert_eq!(snapshot.retain_known(None, Some(&[])), 1);
        assert_eq!(snapshot.sessions[0].session_id, "code");
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
            "no Chat list is fetched without Chat rows"
        );
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
