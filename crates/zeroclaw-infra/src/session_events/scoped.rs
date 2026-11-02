//! A native store handle resolves the active format at each mutation. The
//! immutable handle binds only its session ID; installation format is the backend
//! choice owner and events are the relationship owner.
use super::installation::{InstallationGuard, NativeBackend};
use super::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use zeroclaw_api::session_events::{EventData, EventKind};

/// A host generation reuses physical connections and their disposable
/// indexes. The installation format still selects the backend at each operation;
/// this pool contains neither permissions nor writable session metadata.
type StoreKey = (PathBuf, NativeBackend, u64);

#[derive(Default)]
pub struct EventStorePool {
    stores: Mutex<HashMap<StoreKey, Arc<dyn SessionEventStore>>>,
}
impl EventStorePool {
    /// Called only after the host has drained its installation owners.
    pub fn release_installation(&self, data_dir: &Path) {
        self.stores
            .lock()
            .retain(|(path, _, _), _| path != data_dir);
    }
    pub fn open_session(
        &self,
        data_dir: &Path,
        family: &str,
        lease: Option<Arc<super::ownership::SessionExecutionLease>>,
    ) -> Result<ScopedEventStore, AppendError> {
        if lease
            .as_ref()
            .is_some_and(|lease| !lease.matches(data_dir, family))
        {
            return Err(AppendError::InvalidReference(
                "execution lease scope mismatch".into(),
            ));
        }
        let directory = data_dir.join("sessions");
        let guard = InstallationGuard::acquire(&directory)?;
        let (backend, generation) = guard.active()?;
        let store = self.open_installation(&guard)?;
        // A session ID selects one deterministic event stream. The source
        // marker is metadata on the root event; it is never consulted for
        // routing or enumeration.
        let session_id = SessionId::parse(family.to_owned())
            .map_err(|error| AppendError::InvalidReference(error.to_string()))?;
        let head = store.head(&session_id).map_err(super::upgrade::storage)?;
        if head.sequence == 0 {
            return Err(AppendError::InvalidReference(
                "native session missing".into(),
            ));
        }
        Ok(ScopedEventStore {
            directory,
            session_id,
            backend,
            generation,
            store,
            cli_state_lease: None,
            execution_lease: lease,
        })
    }

    pub fn open_installation(
        &self,
        guard: &InstallationGuard,
    ) -> Result<Arc<dyn SessionEventStore>, AppendError> {
        let (backend, generation) = guard.active()?;
        let data_dir = guard
            .directory()
            .parent()
            .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?;
        let key = (data_dir.to_path_buf(), backend, generation);
        let mut stores = self.stores.lock();
        stores.retain(|(path, format, revision), _| {
            path != data_dir || (*format == backend && *revision == generation)
        });
        if let Some(store) = stores.get(&key) {
            return Ok(store.clone());
        }
        let store = open_event_store(data_dir, backend.name())?;
        stores.insert(key, store.clone());
        Ok(store)
    }
}

pub struct ScopedEventStore {
    directory: PathBuf,
    session_id: SessionId,
    backend: NativeBackend,
    generation: u64,
    store: Arc<dyn SessionEventStore>,
    cli_state_lease: Option<Arc<super::cli_state::CliStateGuard>>,
    execution_lease: Option<Arc<super::ownership::SessionExecutionLease>>,
}
impl ScopedEventStore {
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }
    pub fn open(data_dir: &Path, family: &str) -> Result<Self, AppendError> {
        EventStorePool::default().open_session(data_dir, family, None)
    }
    /// Lease lives as long as the store, including outstanding commit workers
    /// and bound observations after the host drops its execution coordinator.
    pub fn open_with_execution_lease(
        data_dir: &Path,
        family: &str,
        lease: Arc<super::ownership::SessionExecutionLease>,
    ) -> Result<Self, AppendError> {
        if !lease.matches(data_dir, family) {
            return Err(AppendError::InvalidReference(
                "execution lease scope mismatch".into(),
            ));
        }
        let mut store = Self::open(data_dir, family)?;
        store.execution_lease = Some(lease);
        Ok(store)
    }
    /// The store retains the exclusive CLI path lease for every coordinator,
    /// blocking commit worker and escaped bound observer's full lifetime.
    pub fn open_with_cli_lease(
        data_dir: &Path,
        family: &str,
        lease: Arc<super::cli_state::CliStateGuard>,
    ) -> Result<Self, AppendError> {
        let mut store = Self::open(data_dir, family)?;
        let binding = lease.binding().ok_or_else(|| {
            AppendError::InvalidReference("CLI path has no native binding".into())
        })?;
        if binding.session_id != family || binding.data_dir != data_dir {
            return Err(AppendError::InvalidReference(
                "CLI path lease belongs to another installation/source".into(),
            ));
        }
        store.cli_state_lease = Some(lease);
        store.execution_lease = Some(Arc::new(super::ownership::SessionExecutionLease::acquire(
            data_dir, family,
        )?));
        Ok(store)
    }
    fn root(&self, guard: &InstallationGuard) -> Result<SessionId, AppendError> {
        if self.execution_lease.as_ref().is_some_and(|lease| {
            !lease.matches(
                self.directory.parent().unwrap_or(&self.directory),
                self.session_id.as_str(),
            )
        }) {
            return Err(AppendError::InvalidReference(
                "execution lease lost its scope".into(),
            ));
        }
        if let Some(lease) = &self.cli_state_lease
            && lease
                .binding()
                .is_none_or(|binding| binding.session_id != self.session_id.as_str())
        {
            return Err(AppendError::InvalidReference(
                "CLI state ownership binding missing".into(),
            ));
        }
        let (backend, generation) = guard.active()?;
        if backend != self.backend || generation != self.generation {
            return Err(AppendError::InvalidTransition(
                "native writer generation changed".into(),
            ));
        }
        Ok(self.session_id.clone())
    }
    fn require_member(&self, root: &SessionId, request: &AppendRequest) -> Result<(), AppendError> {
        let mut stream = request.session_id.clone();
        let mut seen = HashSet::new();
        for _ in 0..64 {
            if stream == *root {
                return Ok(());
            }
            if !seen.insert(stream.clone()) {
                return Err(AppendError::Corrupt("cyclic session source".into()));
            }
            let mut page = self
                .store
                .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 1)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            let origin = page
                .events
                .pop()
                .or_else(|| {
                    request
                        .events
                        .iter()
                        .find(|event| {
                            event.session_id == stream
                                && event.body.kind() == EventKind::DerivedSessionStarted
                        })
                        .cloned()
                })
                .ok_or_else(|| {
                    AppendError::InvalidReference("child stream has no derivation".into())
                })?;
            if !matches!(origin.body.data, EventData::DerivedSessionStarted { .. }) {
                return Err(AppendError::InvalidReference(
                    "stream belongs to another source".into(),
                ));
            }
            let cause = origin
                .causation_id
                .as_ref()
                .ok_or_else(|| AppendError::InvalidReference("child origin absent".into()))?;
            let parent = self
                .store
                .read_event(cause)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?
                .ok_or_else(|| AppendError::InvalidReference("parent request absent".into()))?;
            if parent.body.kind() != EventKind::ChildInvocationRequested {
                return Err(AppendError::InvalidReference("invalid child origin".into()));
            }
            stream = parent.session_id;
        }
        Err(AppendError::InvalidReference(
            "session source depth exceeded".into(),
        ))
    }
}
impl SessionEventStore for ScopedEventStore {
    fn read_latest_event(
        &self,
        stream: &SessionId,
        branch: &zeroclaw_api::session_events::BranchId,
        turn: Option<&zeroclaw_api::session_events::TurnId>,
        kind: &EventKind,
    ) -> Result<Option<SessionEvent>, ReadError> {
        self.store.read_latest_event(stream, branch, turn, kind)
    }
    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        let guard = InstallationGuard::acquire(&self.directory)?;
        let root = self.root(&guard)?;
        self.require_member(&root, &request)?;
        if self
            .store
            .head(&root)
            .map_err(super::upgrade::storage)?
            .sequence
            == 0
        {
            return Err(AppendError::WriterFenced(
                "session has been discarded".into(),
            ));
        }
        let closed = request
            .events
            .iter()
            .any(|event| matches!(event.body.data, EventData::SessionClosed { .. }));
        let session_id = request.session_id.clone();
        // Hold the same cross-process publication lock through the commit,
        // so freeze cannot race an already-open native handle.
        let receipt = self.store.append_batch(request)?;
        if closed {
            super::temporary::discard_if_closed(
                self.directory.parent().ok_or_else(|| {
                    AppendError::Corrupt("session directory parent missing".into())
                })?,
                self.backend,
                self.store.as_ref(),
                &session_id,
            )?;
        }
        Ok(receipt)
    }
    fn read_event(&self, id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        self.store.read_event(id)
    }
    fn read_page(
        &self,
        stream: &SessionId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        events: usize,
    ) -> Result<EventPage, ReadError> {
        self.store.read_page(stream, after, through, events)
    }
    fn head(&self, stream: &SessionId) -> Result<EventCursor, ReadError> {
        self.store.head(stream)
    }
    fn lookup_receipt(
        &self,
        stream: &SessionId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        self.store.lookup_receipt(stream, key)
    }
    fn read_payload(&self, id: &PayloadId) -> Result<PayloadRead, ReadError> {
        self.store.read_payload(id)
    }
    fn erasure_scope(
        &self,
        targets: &[zeroclaw_api::session_events::StoredPayloadRef],
    ) -> Result<(Vec<SessionId>, Digest), AppendError> {
        self.store.erasure_scope(targets)
    }
}
