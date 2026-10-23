//! A native store handle resolves the active format at each mutation. The
//! immutable handle binds only its family identity; installation format is the backend
//! choice owner and events are the relationship owner.
use super::installation::{InstallationGuard, NativeBackend};
use super::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use zeroclaw_api::session_events::{EventData, EventKind};

/// A host generation reuses physical connections and their disposable
/// indexes. The installation format still selects the backend at each family operation;
/// this pool contains neither permissions nor writable family metadata.
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
    pub fn open_family(
        &self,
        data_dir: &Path,
        family: &str,
        lease: Option<Arc<super::ownership::FamilyExecutionLease>>,
    ) -> Result<FamilyEventStore, AppendError> {
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
        let root_stream = super::installation::families(data_dir, backend)?
            .remove(family)
            .ok_or_else(|| {
                AppendError::InvalidReference("native family identity missing".into())
            })?;
        let store = self.open_installation(&guard)?;
        Ok(FamilyEventStore {
            directory,
            family: family.into(),
            backend,
            generation,
            root_stream,
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

pub struct FamilyEventStore {
    directory: PathBuf,
    family: String,
    backend: NativeBackend,
    generation: u64,
    root_stream: SessionStreamId,
    store: Arc<dyn SessionEventStore>,
    cli_state_lease: Option<Arc<super::cli_state::CliStateGuard>>,
    execution_lease: Option<Arc<super::ownership::FamilyExecutionLease>>,
}
impl FamilyEventStore {
    pub fn root_stream(&self) -> &SessionStreamId {
        &self.root_stream
    }
    pub fn open(data_dir: &Path, family: &str) -> Result<Self, AppendError> {
        EventStorePool::default().open_family(data_dir, family, None)
    }
    /// Lease lives as long as the store, including outstanding commit workers
    /// and bound observations after the host drops its execution coordinator.
    pub fn open_with_execution_lease(
        data_dir: &Path,
        family: &str,
        lease: Arc<super::ownership::FamilyExecutionLease>,
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
        if binding.family != family || binding.data_dir != data_dir {
            return Err(AppendError::InvalidReference(
                "CLI path lease belongs to another installation/family".into(),
            ));
        }
        store.cli_state_lease = Some(lease);
        store.execution_lease = Some(Arc::new(super::ownership::FamilyExecutionLease::acquire(
            data_dir, family,
        )?));
        Ok(store)
    }
    fn root(&self, guard: &InstallationGuard) -> Result<SessionStreamId, AppendError> {
        if self.execution_lease.as_ref().is_some_and(|lease| {
            !lease.matches(
                self.directory.parent().unwrap_or(&self.directory),
                &self.family,
            )
        }) {
            return Err(AppendError::InvalidReference(
                "execution lease lost its scope".into(),
            ));
        }
        if let Some(lease) = &self.cli_state_lease
            && lease
                .binding()
                .is_none_or(|binding| binding.family != self.family)
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
        Ok(self.root_stream.clone())
    }
    fn require_member(
        &self,
        root: &SessionStreamId,
        request: &AppendRequest,
    ) -> Result<(), AppendError> {
        let mut stream = request.stream_id.clone();
        let mut seen = HashSet::new();
        for _ in 0..64 {
            if stream == *root {
                return Ok(());
            }
            if !seen.insert(stream.clone()) {
                return Err(AppendError::Corrupt("cyclic session family".into()));
            }
            let mut page = self
                .store
                .read_page(
                    &stream,
                    &EventCursor::beginning(stream.clone()),
                    None,
                    1,
                    MAX_PAGE_BYTES,
                )
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            let origin = page
                .events
                .pop()
                .or_else(|| {
                    request
                        .events
                        .iter()
                        .find(|event| {
                            event.stream_id == stream
                                && event.body.kind() == EventKind::DerivedSessionStarted
                        })
                        .cloned()
                })
                .ok_or_else(|| {
                    AppendError::InvalidReference("child stream has no derivation".into())
                })?;
            if !matches!(origin.body.data, EventData::DerivedSessionStarted { .. }) {
                return Err(AppendError::InvalidReference(
                    "stream belongs to another family".into(),
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
            stream = parent.stream_id;
        }
        Err(AppendError::InvalidReference(
            "session family depth exceeded".into(),
        ))
    }
}
impl SessionEventStore for FamilyEventStore {
    fn read_latest_event(
        &self,
        stream: &SessionStreamId,
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
        // Hold the same cross-process publication lock through the commit,
        // so freeze cannot race an already-open native handle.
        self.store.append_batch(request)
    }
    fn read_event(&self, id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        self.store.read_event(id)
    }
    fn read_page(
        &self,
        stream: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        events: usize,
        bytes: usize,
    ) -> Result<EventPage, ReadError> {
        self.store.read_page(stream, after, through, events, bytes)
    }
    fn head(&self, stream: &SessionStreamId) -> Result<EventCursor, ReadError> {
        self.store.head(stream)
    }
    fn lookup_receipt(
        &self,
        stream: &SessionStreamId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        self.store.lookup_receipt(stream, key)
    }
    fn read_payload_bounded(&self, id: &PayloadId, bytes: usize) -> Result<PayloadRead, ReadError> {
        self.store.read_payload_bounded(id, bytes)
    }
    fn erasure_scope(
        &self,
        targets: &[zeroclaw_api::session_events::StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
        self.store.erasure_scope(targets)
    }
}
