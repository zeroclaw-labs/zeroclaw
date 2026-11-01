//! Chunked uploads for the RPC transport (`file/upload/{begin,chunk,commit}`).
//!
//! A payload too large for one NDJSON frame arrives as ordered chunks and is
//! staged in memory until the client commits it. Staged uploads belong to the
//! connection that began them: another connection cannot name them, and they
//! are released when the connection closes. Three bounds keep staging from
//! becoming a memory sink: a per-connection upload count, an idle deadline,
//! and a process-wide byte budget that every staged byte is charged against
//! when the upload begins.
//!
//! The idle deadline is enforced by the budget as well as by the owning
//! connection. When a reservation does not fit, the budget reclaims every
//! upload idle past the deadline, freeing its bytes and its reservation,
//! even while the connection that began it stays open and silent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS;

use super::attachments::MAX_FILE_BYTES;

/// Largest decoded chunk accepted per `file/upload/chunk`. Its base64 form
/// stays well under the local transport's frame limit.
pub const UPLOAD_CHUNK_BYTES: u64 = 1024 * 1024;

/// Uploads one connection may stage at once.
pub const MAX_UPLOADS_PER_CONNECTION: usize = 4;

/// An upload with no `begin`, `chunk`, or `commit` activity for this long is
/// discarded: by its connection the next time that connection touches
/// uploads, or by the budget when another upload needs the space.
pub const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Bytes all connections together may hold in staged uploads, counting
/// both the payload and the metadata each upload retains.
pub const PROCESS_STAGED_BYTES: u64 = 256 * 1024 * 1024;

/// Longest filename, in bytes, an upload may carry. The name is only a
/// display label (storage is content-addressed); this is the usual limit
/// for one path component.
pub const MAX_UPLOAD_FILENAME_BYTES: usize = 255;

fn invalid(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: INVALID_PARAMS,
        message: message.into(),
        data: None,
    }
}

fn expired(upload_id: &str) -> JsonRpcError {
    invalid(format!("Unknown or expired upload `{upload_id}`"))
}

/// A shared ceiling on staged bytes. Space is reserved for an upload's full
/// declared size when it begins, so chunks never fail for lack of budget
/// halfway through.
#[derive(Debug)]
pub struct UploadBudget {
    limit: u64,
    used: AtomicU64,
    /// Every live upload's slot, so idle ones can be reclaimed without their
    /// connection's help.
    slots: Mutex<Vec<Weak<Slot>>>,
}

impl UploadBudget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicU64::new(0),
            slots: Mutex::new(Vec::new()),
        })
    }

    /// The budget shared by every connection in this process.
    pub fn process() -> Arc<Self> {
        static PROCESS: OnceLock<Arc<UploadBudget>> = OnceLock::new();
        PROCESS
            .get_or_init(|| UploadBudget::new(PROCESS_STAGED_BYTES))
            .clone()
    }

    fn try_charge(&self, bytes: u64) -> bool {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= self.limit)
            })
            .is_ok()
    }

    fn lock_slots(&self) -> MutexGuard<'_, Vec<Weak<Slot>>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserve room for a new upload's payload and the metadata it keeps,
    /// reclaiming idle uploads first if the budget is full.
    fn reserve(
        self: &Arc<Self>,
        now: Instant,
        payload: u64,
        meta: UploadMeta,
    ) -> Option<Arc<Slot>> {
        let bytes = payload.saturating_add(meta.retained_bytes());
        if !self.try_charge(bytes) {
            self.reclaim_idle(now);
            if !self.try_charge(bytes) {
                return None;
            }
        }
        let slot = Arc::new(Slot {
            budget: Arc::clone(self),
            reserved: bytes,
            state: Mutex::new(SlotState {
                meta: Some(meta),
                data: Vec::with_capacity(usize::try_from(payload).unwrap_or(0)),
                last_activity: now,
                reclaimed: false,
            }),
        });
        let mut slots = self.lock_slots();
        slots.retain(|slot| slot.strong_count() > 0);
        slots.push(Arc::downgrade(&slot));
        Some(slot)
    }

    /// Release every upload idle past [`UPLOAD_IDLE_TIMEOUT`], wherever its
    /// connection is.
    fn reclaim_idle(&self, now: Instant) {
        let live: Vec<Arc<Slot>> = {
            let mut slots = self.lock_slots();
            slots.retain(|slot| slot.strong_count() > 0);
            slots.iter().filter_map(Weak::upgrade).collect()
        };
        for slot in live {
            let mut state = slot.lock_state();
            if state.is_idle(now) {
                slot.release(&mut state);
            }
        }
    }

    #[cfg(test)]
    fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }
}

/// The exact session incarnation an upload was begun for: the live session
/// by id and generation, the principal that owns it, and the agent it runs.
/// Commit writes only into this incarnation, so a session closed and
/// recreated under the same id, re-owned, or moved to another agent while
/// the upload was staged is refused rather than written into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTarget {
    pub session_id: String,
    pub generation: u64,
    pub owner: Option<String>,
    pub agent_alias: String,
}

impl UploadTarget {
    fn retained_bytes(&self) -> usize {
        self.session_id.len() + self.agent_alias.len() + self.owner.as_ref().map_or(0, String::len)
    }
}

/// What an upload remembers about itself until it is committed.
#[derive(Debug)]
struct UploadMeta {
    target: UploadTarget,
    filename: String,
    expected_sha256: Option<String>,
}

impl UploadMeta {
    /// Heap bytes this metadata keeps resident, charged to the budget with
    /// the payload so metadata cannot stand in for bytes it never counted.
    fn retained_bytes(&self) -> u64 {
        let len = self.target.retained_bytes()
            + self.filename.len()
            + self.expected_sha256.as_ref().map_or(0, String::len);
        len as u64
    }
}

/// One upload's metadata, staged bytes, and budget reservation, shared
/// between the connection that owns the upload and the budget that may
/// reclaim it. Reclaiming frees all of it; the owning connection keeps only
/// the upload id until it next touches uploads.
#[derive(Debug)]
struct Slot {
    budget: Arc<UploadBudget>,
    reserved: u64,
    state: Mutex<SlotState>,
}

#[derive(Debug)]
struct SlotState {
    /// Taken on commit, cleared on reclaim.
    meta: Option<UploadMeta>,
    data: Vec<u8>,
    last_activity: Instant,
    /// Set once the reservation has been returned; the upload is dead.
    reclaimed: bool,
}

impl SlotState {
    fn is_idle(&self, now: Instant) -> bool {
        !self.reclaimed && now.saturating_duration_since(self.last_activity) >= UPLOAD_IDLE_TIMEOUT
    }
}

impl Slot {
    fn lock_state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Return the reservation and free the bytes, once.
    fn release(&self, state: &mut SlotState) {
        if state.reclaimed {
            return;
        }
        state.reclaimed = true;
        state.meta = None;
        state.data = Vec::new();
        self.budget.used.fetch_sub(self.reserved, Ordering::AcqRel);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if !state.reclaimed {
            state.reclaimed = true;
            self.budget.used.fetch_sub(self.reserved, Ordering::AcqRel);
        }
    }
}

/// A fully received upload, handed to the commit path for persistence.
#[derive(Debug)]
pub struct CompletedUpload {
    pub target: UploadTarget,
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
struct StagedUpload {
    declared_size: u64,
    slot: Arc<Slot>,
}

/// What `begin` needs to know about an upload, after the dispatcher has
/// resolved the session incarnation and authorized it.
pub struct BeginRequest {
    pub target: UploadTarget,
    pub filename: Option<String>,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

/// The uploads one connection has begun and not yet committed.
#[derive(Debug)]
pub struct UploadStaging {
    uploads: HashMap<String, StagedUpload>,
    budget: Arc<UploadBudget>,
}

impl Default for UploadStaging {
    fn default() -> Self {
        Self::new(UploadBudget::process())
    }
}

impl UploadStaging {
    pub fn new(budget: Arc<UploadBudget>) -> Self {
        Self {
            uploads: HashMap::new(),
            budget,
        }
    }

    /// Drop uploads that are idle past the deadline or were already
    /// reclaimed by the budget.
    fn evict_idle(&mut self, now: Instant) {
        self.uploads.retain(|_, upload| {
            let state = upload.slot.lock_state();
            !state.reclaimed && !state.is_idle(now)
        });
    }

    /// Stage a new upload and return its id.
    pub fn begin(&mut self, now: Instant, request: BeginRequest) -> Result<String, JsonRpcError> {
        self.evict_idle(now);
        if request.size_bytes > MAX_FILE_BYTES {
            return Err(invalid(format!(
                "Upload of {} bytes exceeds the {} MB file limit",
                request.size_bytes,
                MAX_FILE_BYTES / (1024 * 1024)
            )));
        }
        if let Some(filename) = &request.filename
            && filename.len() > MAX_UPLOAD_FILENAME_BYTES
        {
            return Err(invalid(format!(
                "`filename` is {} bytes; the limit is {MAX_UPLOAD_FILENAME_BYTES}",
                filename.len()
            )));
        }
        let expected_sha256 = request
            .sha256
            .map(|hex| normalize_sha256(&hex))
            .transpose()?;
        if self.uploads.len() >= MAX_UPLOADS_PER_CONNECTION {
            return Err(invalid(format!(
                "This connection already has {MAX_UPLOADS_PER_CONNECTION} uploads in progress; \
                 commit them or let them expire before beginning another"
            )));
        }
        let meta = UploadMeta {
            target: request.target,
            filename: request.filename.unwrap_or_else(|| "upload".to_string()),
            expected_sha256,
        };
        let slot = self
            .budget
            .reserve(now, request.size_bytes, meta)
            .ok_or_else(|| {
                invalid(
                    "The daemon is staging too many uploads to accept this one now; retry after \
                 in-progress uploads finish",
                )
            })?;
        let upload_id = uuid::Uuid::new_v4().simple().to_string();
        self.uploads.insert(
            upload_id.clone(),
            StagedUpload {
                declared_size: request.size_bytes,
                slot,
            },
        );
        Ok(upload_id)
    }

    /// Append one chunk at `offset` and return the bytes received so far.
    pub fn chunk(
        &mut self,
        now: Instant,
        upload_id: &str,
        offset: u64,
        chunk: &[u8],
    ) -> Result<u64, JsonRpcError> {
        self.evict_idle(now);
        let upload = self
            .uploads
            .get(upload_id)
            .ok_or_else(|| expired(upload_id))?;
        let chunk_len = chunk.len() as u64;
        if chunk_len == 0 || chunk_len > UPLOAD_CHUNK_BYTES {
            return Err(invalid(format!(
                "A chunk must carry between 1 and {UPLOAD_CHUNK_BYTES} decoded bytes"
            )));
        }
        let mut state = upload.slot.lock_state();
        if state.reclaimed {
            return Err(expired(upload_id));
        }
        let received = state.data.len() as u64;
        let end = offset
            .checked_add(chunk_len)
            .ok_or_else(|| invalid("Chunk offset overflows"))?;
        if offset < received {
            // A retry of an accepted chunk is harmless only if it repeats the
            // same bytes; anything else would rewrite received data.
            let same = end <= received
                && usize::try_from(offset)
                    .ok()
                    .and_then(|start| state.data.get(start..start + chunk.len()))
                    == Some(chunk);
            if same {
                state.last_activity = now;
                return Ok(received);
            }
            return Err(invalid(format!(
                "Chunk at offset {offset} overlaps received data; the next offset is {received}"
            )));
        }
        if offset > received {
            return Err(invalid(format!(
                "Chunk at offset {offset} leaves a gap; the next offset is {received}"
            )));
        }
        if end > upload.declared_size {
            return Err(invalid(format!(
                "Chunk ends at byte {end}, past the declared size of {}",
                upload.declared_size
            )));
        }
        state.data.extend_from_slice(chunk);
        state.last_activity = now;
        Ok(end)
    }

    /// Remove a fully received upload for persistence. An incomplete or
    /// mismatched upload stays staged so the client can finish or retry it.
    pub fn take_complete(
        &mut self,
        now: Instant,
        upload_id: &str,
    ) -> Result<CompletedUpload, JsonRpcError> {
        use sha2::{Digest, Sha256};

        self.evict_idle(now);
        let upload = self
            .uploads
            .get(upload_id)
            .ok_or_else(|| expired(upload_id))?;
        let completed = {
            let mut state = upload.slot.lock_state();
            if state.reclaimed {
                return Err(expired(upload_id));
            }
            state.last_activity = now;
            let received = state.data.len() as u64;
            if received != upload.declared_size {
                return Err(invalid(format!(
                    "Upload has {received} of {} declared bytes; send the rest before committing",
                    upload.declared_size
                )));
            }
            let matches = state
                .meta
                .as_ref()
                .and_then(|meta| meta.expected_sha256.as_ref())
                .is_none_or(|expected| *expected == format!("{:x}", Sha256::digest(&state.data)));
            if matches {
                let bytes = std::mem::take(&mut state.data);
                state.meta.take().map(|meta| CompletedUpload {
                    target: meta.target,
                    filename: meta.filename,
                    bytes,
                })
            } else {
                None
            }
        };
        // Removing the upload drops its slot, which returns the reservation.
        self.uploads.remove(upload_id);
        completed.ok_or_else(|| {
            // The bytes will never match now; the upload is discarded rather
            // than kept charged against the budget.
            invalid("Upload content does not match the SHA-256 declared at begin; begin again")
        })
    }

    /// The session incarnation an upload was begun for, for
    /// re-authorization before commit.
    pub fn binding(&self, upload_id: &str) -> Option<UploadTarget> {
        let upload = self.uploads.get(upload_id)?;
        let state = upload.slot.lock_state();
        state.meta.as_ref().map(|meta| meta.target.clone())
    }
}

fn normalize_sha256(hex: &str) -> Result<String, JsonRpcError> {
    let hex = hex.trim().to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("`sha256` must be 64 hexadecimal characters"));
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(session_id: &str, agent_alias: &str, owner: Option<&str>) -> UploadTarget {
        UploadTarget {
            session_id: session_id.into(),
            generation: 1,
            owner: owner.map(Into::into),
            agent_alias: agent_alias.into(),
        }
    }

    fn request(size_bytes: u64) -> BeginRequest {
        BeginRequest {
            target: target("s1", "default", Some("alice")),
            filename: Some("notes.txt".into()),
            size_bytes,
            sha256: None,
        }
    }

    /// Bytes of metadata `request` retains: session, agent, owner, and
    /// filename.
    const REQUEST_META_BYTES: u64 =
        ("s1".len() + "default".len() + "alice".len() + "notes.txt".len()) as u64;

    /// A request that retains no metadata, for exact budget arithmetic.
    fn bare(size_bytes: u64) -> BeginRequest {
        BeginRequest {
            target: target("", "", None),
            filename: Some(String::new()),
            size_bytes,
            sha256: None,
        }
    }

    fn staging(limit: u64) -> (UploadStaging, Arc<UploadBudget>) {
        let budget = UploadBudget::new(limit);
        (UploadStaging::new(Arc::clone(&budget)), budget)
    }

    #[test]
    fn ordered_chunks_assemble_the_declared_payload() {
        let (mut staging, budget) = staging(1024);
        let now = Instant::now();
        let id = staging.begin(now, request(6)).unwrap();
        assert_eq!(
            budget.used(),
            6 + REQUEST_META_BYTES,
            "the payload and the retained metadata are both charged"
        );
        assert_eq!(staging.chunk(now, &id, 0, b"abc").unwrap(), 3);
        assert_eq!(staging.chunk(now, &id, 3, b"def").unwrap(), 6);
        let done = staging.take_complete(now, &id).unwrap();
        assert_eq!(done.bytes, b"abcdef");
        assert_eq!(done.filename, "notes.txt");
        assert_eq!(budget.used(), 0, "commit returns the reservation");
    }

    #[test]
    fn gaps_and_rewrites_are_refused_but_identical_retries_are_acknowledged() {
        let (mut staging, _) = staging(1024);
        let now = Instant::now();
        let id = staging.begin(now, request(6)).unwrap();
        staging.chunk(now, &id, 0, b"abc").unwrap();

        let gap = staging.chunk(now, &id, 4, b"ef").unwrap_err();
        assert!(gap.message.contains("next offset is 3"), "{}", gap.message);

        let rewrite = staging.chunk(now, &id, 0, b"xyz").unwrap_err();
        assert!(rewrite.message.contains("overlaps"), "{}", rewrite.message);

        assert_eq!(staging.chunk(now, &id, 0, b"abc").unwrap(), 3);
        assert_eq!(staging.chunk(now, &id, 1, b"bc").unwrap(), 3);
    }

    #[test]
    fn chunks_past_the_declared_size_or_over_the_chunk_limit_are_refused() {
        let (mut staging, _) = staging(u64::MAX);
        let now = Instant::now();
        let id = staging.begin(now, request(4)).unwrap();
        assert!(staging.chunk(now, &id, 0, b"abcde").is_err());
        assert!(staging.chunk(now, &id, 0, b"").is_err());

        let big = staging.begin(now, request(UPLOAD_CHUNK_BYTES + 1)).unwrap();
        let oversized = vec![0_u8; usize::try_from(UPLOAD_CHUNK_BYTES).unwrap() + 1];
        assert!(staging.chunk(now, &big, 0, &oversized).is_err());
    }

    #[test]
    fn declared_size_is_bounded_by_the_file_limit() {
        let (mut staging, budget) = staging(u64::MAX);
        let err = staging
            .begin(Instant::now(), request(MAX_FILE_BYTES + 1))
            .unwrap_err();
        assert!(err.message.contains("file limit"), "{}", err.message);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn commit_requires_every_declared_byte() {
        let (mut staging, _) = staging(1024);
        let now = Instant::now();
        let id = staging.begin(now, request(6)).unwrap();
        staging.chunk(now, &id, 0, b"abc").unwrap();
        let err = staging.take_complete(now, &id).unwrap_err();
        assert!(err.message.contains("3 of 6"), "{}", err.message);
        staging.chunk(now, &id, 3, b"def").unwrap();
        assert!(staging.take_complete(now, &id).is_ok());
    }

    #[test]
    fn declared_sha256_is_verified_at_commit() {
        use sha2::{Digest, Sha256};
        let (mut staging, budget) = staging(1024);
        let now = Instant::now();
        let good = format!("{:X}", Sha256::digest(b"abc"));

        let mut req = request(3);
        req.sha256 = Some(good);
        let id = staging.begin(now, req).unwrap();
        staging.chunk(now, &id, 0, b"abc").unwrap();
        assert!(staging.take_complete(now, &id).is_ok());

        let mut req = request(3);
        req.sha256 = Some(format!("{:x}", Sha256::digest(b"xyz")));
        let id = staging.begin(now, req).unwrap();
        staging.chunk(now, &id, 0, b"abc").unwrap();
        assert!(staging.take_complete(now, &id).is_err());
        assert_eq!(budget.used(), 0, "a mismatched upload is discarded");
        assert!(staging.take_complete(now, &id).is_err());

        let mut req = request(3);
        req.sha256 = Some("not-hex".into());
        assert!(staging.begin(now, req).is_err());
    }

    #[test]
    fn a_connection_may_stage_only_a_bounded_number_of_uploads() {
        let (mut staging, _) = staging(u64::MAX);
        let now = Instant::now();
        for _ in 0..MAX_UPLOADS_PER_CONNECTION {
            staging.begin(now, request(1)).unwrap();
        }
        let err = staging.begin(now, request(1)).unwrap_err();
        assert!(err.message.contains("in progress"), "{}", err.message);
    }

    #[test]
    fn the_shared_budget_bounds_staged_bytes_across_connections() {
        let budget = UploadBudget::new(10);
        let mut first = UploadStaging::new(Arc::clone(&budget));
        let mut second = UploadStaging::new(Arc::clone(&budget));
        let now = Instant::now();
        first.begin(now, bare(8)).unwrap();
        let err = second.begin(now, bare(3)).unwrap_err();
        assert!(err.message.contains("too many uploads"), "{}", err.message);
        second.begin(now, bare(2)).unwrap();

        drop(first);
        assert_eq!(
            budget.used(),
            2,
            "dropping a connection's staging releases it"
        );
        second.begin(now, bare(8)).unwrap();
    }

    #[test]
    fn idle_uploads_expire_and_release_their_budget() {
        let (mut staging, budget) = staging(1024);
        let start = Instant::now();
        let id = staging.begin(start, request(5)).unwrap();
        let later = start + UPLOAD_IDLE_TIMEOUT + Duration::from_secs(1);
        let err = staging.chunk(later, &id, 0, b"a").unwrap_err();
        assert!(err.message.contains("expired"), "{}", err.message);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn an_idle_upload_on_an_open_connection_frees_the_budget_for_others() {
        let budget = UploadBudget::new(10);
        let mut silent = UploadStaging::new(Arc::clone(&budget));
        let mut other = UploadStaging::new(Arc::clone(&budget));
        let start = Instant::now();

        // The silent connection reserves most of the budget, then never
        // touches uploads again while it stays open.
        let held = silent.begin(start, bare(8)).unwrap();
        silent.chunk(start, &held, 0, b"abc").unwrap();
        assert!(other.begin(start, bare(3)).is_err(), "the budget is full");

        // Past the idle deadline, another connection's reservation reclaims
        // it without any help from the silent connection.
        let later = start + UPLOAD_IDLE_TIMEOUT + Duration::from_secs(1);
        other.begin(later, bare(3)).unwrap();
        assert_eq!(budget.used(), 3, "only the new reservation is charged");

        // The reclaimed upload is gone for its owner too, and dropping the
        // owner does not release the reservation a second time.
        let err = silent.chunk(later, &held, 3, b"d").unwrap_err();
        assert!(err.message.contains("expired"), "{}", err.message);
        drop(silent);
        assert_eq!(budget.used(), 3);
    }

    #[test]
    fn an_active_upload_is_not_reclaimed() {
        let budget = UploadBudget::new(10);
        let mut busy = UploadStaging::new(Arc::clone(&budget));
        let mut other = UploadStaging::new(Arc::clone(&budget));
        let start = Instant::now();
        let id = busy.begin(start, bare(8)).unwrap();
        let recent = start + UPLOAD_IDLE_TIMEOUT - Duration::from_secs(1);
        busy.chunk(recent, &id, 0, b"a").unwrap();

        let later = start + UPLOAD_IDLE_TIMEOUT + Duration::from_secs(1);
        assert!(
            other.begin(later, bare(3)).is_err(),
            "an upload touched within the deadline keeps its reservation"
        );
        assert_eq!(busy.chunk(later, &id, 1, b"b").unwrap(), 2);
    }

    #[test]
    fn a_filename_over_the_limit_is_refused_before_staging() {
        let (mut staging, budget) = staging(u64::MAX);
        let now = Instant::now();
        let mut req = request(0);
        req.filename = Some("a".repeat(MAX_UPLOAD_FILENAME_BYTES + 1));
        let err = staging.begin(now, req).unwrap_err();
        assert!(err.message.contains("filename"), "{}", err.message);
        assert_eq!(budget.used(), 0, "nothing is staged or charged");

        let mut req = request(0);
        req.filename = Some("a".repeat(MAX_UPLOAD_FILENAME_BYTES));
        staging.begin(now, req).unwrap();
    }

    #[test]
    fn retained_metadata_is_charged_even_for_empty_payloads() {
        // The case a zero-byte payload used to hide: metadata was resident
        // but never counted. Oversized session ids stand in for any retained
        // field, since the filename is capped separately.
        let big = |size: u64| BeginRequest {
            target: target(&"s".repeat(4000), "", None),
            filename: Some(String::new()),
            size_bytes: size,
            sha256: None,
        };
        let budget = UploadBudget::new(10_000);
        let mut owners: Vec<UploadStaging> = (0..3)
            .map(|_| UploadStaging::new(Arc::clone(&budget)))
            .collect();
        let now = Instant::now();
        owners[0].begin(now, big(0)).unwrap();
        owners[1].begin(now, big(0)).unwrap();
        assert_eq!(budget.used(), 8000);
        let err = owners[2].begin(now, big(0)).unwrap_err();
        assert!(err.message.contains("too many uploads"), "{}", err.message);

        owners.clear();
        assert_eq!(
            budget.used(),
            0,
            "dropping the owners releases the metadata charge"
        );
    }

    #[test]
    fn the_owner_binding_is_charged_and_handed_back_at_commit() {
        let (mut staging, budget) = staging(10_000);
        let now = Instant::now();
        let mut req = bare(1);
        req.target.owner = Some("o".repeat(3000));
        req.target.generation = 7;
        let id = staging.begin(now, req).unwrap();
        assert_eq!(budget.used(), 1 + 3000, "the retained owner is charged");
        let bound = staging.binding(&id).expect("staged");
        assert_eq!(bound.generation, 7);
        assert_eq!(bound.owner.as_deref().map(str::len), Some(3000));
        staging.chunk(now, &id, 0, b"x").unwrap();
        let done = staging.take_complete(now, &id).unwrap();
        assert_eq!(done.target, bound, "commit carries the begin-time binding");
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn reclaiming_an_idle_upload_frees_its_metadata() {
        let budget = UploadBudget::new(5000);
        let mut silent = UploadStaging::new(Arc::clone(&budget));
        let mut other = UploadStaging::new(Arc::clone(&budget));
        let start = Instant::now();
        let mut held_req = bare(0);
        held_req.target.session_id = "s".repeat(4000);
        let held = silent.begin(start, held_req).unwrap();
        assert!(silent.binding(&held).is_some());

        let later = start + UPLOAD_IDLE_TIMEOUT + Duration::from_secs(1);
        let mut next = bare(0);
        next.target.session_id = "t".repeat(2000);
        other.begin(later, next).unwrap();
        assert_eq!(
            budget.used(),
            2000,
            "the idle upload's metadata is released"
        );
        assert!(
            silent.binding(&held).is_none(),
            "the silent owner no longer holds the reclaimed metadata"
        );
    }

    #[test]
    fn upload_ids_are_scoped_to_their_staging() {
        let (mut owner, _) = staging(1024);
        let (mut other, _) = staging(1024);
        let now = Instant::now();
        let id = owner.begin(now, request(1)).unwrap();
        assert!(other.chunk(now, &id, 0, b"a").is_err());
        assert!(other.take_complete(now, &id).is_err());
        assert!(other.binding(&id).is_none());
    }
}
