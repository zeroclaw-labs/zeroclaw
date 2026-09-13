//! JSONL append-only writer + rolling rotation.
//!
//! RAM contract: a single event lands in two allocations (the JSON line
//! that goes to disk + the `serde_json::Value` clone that goes to the
//! broadcast hook). Rolling rotation streams through `BufReader::lines`
//! into a temp file rather than slurping the whole file into a `String`.
//!
//! ## Hot-path write concurrency model
//!
//! Disk persistence runs on a dedicated `std::thread` named
//! `zeroclaw-log-writer` so `record_event` does not block on file I/O
//! or fsync. The async runtime emits an event by serializing it once
//! and `try_send`-ing onto a bounded `std::sync::mpsc::sync_channel`;
//! when the channel is full (worker is slow or disk is stalled) the
//! event is dropped with a `tracing::warn!` so a single slow disk
//! cannot wedge an agent turn. The worker re-opens the active file
//! per write (matching the prior single-threaded semantics — required
//! because rolling trim and size-based rotation rename the file out
//! from under an open handle), runs the rotation hooks inline, and
//! calls `sync_all` on a periodic cadence (every
//! `SYNC_EVERY_N_WRITES` writes or `SYNC_INTERVAL` of wall-clock
//! time, whichever comes first). This trades per-event durability
//! (the prior behaviour was `sync_data` after every write) for bounded
//! write latency: a process crash may lose up to one sync interval of
//! pending writes.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

use crate::broadcast::current_broadcast_hook;
use crate::config::{LlmRequestPayloadPolicy, LogConfig, ResolvedPolicy, StoragePolicy};
use crate::event::LogEvent;
use crate::migrate;
use crate::observer_bridge;
use crate::reader::{ArchiveOrder, SEQ_WIDTH, list_archives, split_base_ext};
use anyhow::{Context, Result};
use serde_json::Value;

/// Top-level marker stamped onto a broadcast frame when it carries
/// `ephemeral_attributes` (short-lived pairing secrets deep-merged into the
/// live copy). Broadcast consumers use it to withhold the frame from any
/// stream that is not bearer-authenticated — the frame's secrets must fail
/// closed rather than ride an unauthenticated `/api/events` subscriber. It is
/// stamped only on the broadcast copy (never the persisted value, which drops
/// `ephemeral_attributes` via `serde(skip)`) and is stripped by the SSE layer
/// before delivery, so the public event shape is unchanged.
pub const EPHEMERAL_BROADCAST_MARKER: &str = "_ephemeral_credentials";

/// Capacity of the bounded mpsc between `record_event` and the worker.
/// Sized for high-throughput agent turns (each turn can emit 20-100 events)
/// while keeping the queue's RSS footprint bounded. When the queue is full
/// the producer drops the event with a `tracing::warn!` rather than
/// blocking the async task.
const QUEUE_CAPACITY: usize = 1024;

/// Worker calls `sync_all` after every N successful writes (in addition
/// to the wall-clock cadence below). Tuned so a steady-state event stream
/// of 1000 events/sec still gets ~10 fsyncs/sec.
const SYNC_EVERY_N_WRITES: u64 = 100;

/// Worker calls `sync_all` at least this often in wall-clock time even if
/// `SYNC_EVERY_N_WRITES` has not been reached. Bounds the data-loss window
/// under a slow trickle of events.
const SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum time the worker blocks on `recv_timeout` between idle ticks.
/// Kept short so shutdown is prompt and the periodic sync interval is
/// honoured even when no events are flowing.
const IDLE_TICK: Duration = Duration::from_millis(50);

/// How long re-init waits before warning that the previous writer is slow to
/// stop. The warning is not permission to install a second writer for the same
/// path: reload continues waiting to preserve exclusive ownership.
const SHUTDOWN_WARN_AFTER: Duration = Duration::from_millis(500);

/// A unit of work sent from the async runtime to the disk-persistence
/// worker. The `Value` payload is unavoidable because we serialize once
/// on the producer side to keep the queue small.
enum WriterJob {
    /// Serialize-and-write a single event to the log file.
    Write(Value),
    /// Block until the worker has drained all previously-queued jobs and
    /// completed a `sync_all`. Acks via a rendezvous channel.
    Flush(SyncSender<Result<()>>),
    /// Drain all earlier jobs, sync the active file, and exit. Re-init uses
    /// the acknowledgement to establish a quiescent migration boundary.
    Shutdown(SyncSender<()>),
    #[cfg(test)]
    Pause {
        entered: SyncSender<()>,
        release: Receiver<()>,
    },
}

/// Set when the worker thread has exited (panic or normal). Used by
/// `flush_for_test` to short-circuit on a dead worker rather than block.
type WorkerDead = Arc<AtomicBool>;

/// Per-worker state (no `tx` — the worker is the consumer, not a producer).
/// The `policy` and `worker_dead` fields are shared with the producer-facing
/// [`WriterState`].
struct WorkerState {
    policy: ResolvedPolicy,
    worker_dead: WorkerDead,
    /// Non-empty JSONL lines written to the *current* active segment since
    /// the last rotation. Reset to `0` whenever `rotate_active` runs
    /// (date, size, or entry-count trigger). Seeded at worker startup by
    /// counting the existing active file so a restart resumes the correct
    /// count instead of rotating prematurely (or too late) after reload.
    line_count: AtomicU64,
    /// Sequence number for the next archive this writer creates.
    ///
    /// Stamped into the archive name so segment order is fixed at write time
    /// rather than inferred by a reader from mtimes. Seeded at startup from
    /// the highest number already on disk, so the series keeps increasing
    /// across restarts and reloads even though nothing persists it directly.
    next_archive_seq: AtomicU64,
}

/// Producer-facing state. The `tx` sender is NOT shared with the worker
/// so the channel's [`Disconnected`](std::sync::mpsc::TrySendError::Disconnected)
/// exit path can fire once the last producer drops their sender.
struct WriterState {
    policy: ResolvedPolicy,
    tx: SyncSender<WriterJob>,
    worker_dead: WorkerDead,
}

static WRITER: OnceLock<parking_lot::RwLock<Option<Arc<WriterState>>>> = OnceLock::new();
static REINIT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static HANDOFF_BUFFER: OnceLock<parking_lot::Mutex<Option<Vec<Value>>>> = OnceLock::new();

fn slot() -> &'static parking_lot::RwLock<Option<Arc<WriterState>>> {
    WRITER.get_or_init(|| parking_lot::RwLock::new(None))
}

fn current_state() -> Option<Arc<WriterState>> {
    slot().read().clone()
}

pub fn init_from_config(config: &LogConfig, workspace_dir: &Path) {
    init_from_config_with_migration(
        config,
        workspace_dir,
        migrate::migrate_legacy_jsonl_in_place,
    );
}

fn init_from_config_with_migration<F>(config: &LogConfig, workspace_dir: &Path, migrate_file: F)
where
    F: FnOnce(&Path) -> Result<()>,
{
    init_from_config_with_migration_and_shutdown_warning(
        config,
        workspace_dir,
        migrate_file,
        SHUTDOWN_WARN_AFTER,
    );
}

fn init_from_config_with_migration_and_shutdown_warning<F>(
    config: &LogConfig,
    workspace_dir: &Path,
    migrate_file: F,
    shutdown_warn_after: Duration,
) where
    F: FnOnce(&Path) -> Result<()>,
{
    let _reinit_guard = REINIT_LOCK.lock();
    let policy = ResolvedPolicy::from_config(config, workspace_dir);

    // Route writes emitted during reload into a temporary buffer. Taking this
    // lock before removing the old writer closes the race with producers that
    // are about to clone its sender.
    *handoff_buffer().lock() = Some(Vec::new());

    shutdown_current_writer(shutdown_warn_after);

    if policy.storage.is_enabled()
        && policy.path.exists()
        && let Err(err) = migrate_file(&policy.path)
    {
        tracing::warn!(
            target: "zeroclaw_log",
            error = ?err,
            path = %policy.path.display(),
            "log: legacy JSONL migration failed; daemon continuing with mixed-shape file"
        );
    }
    let (tx, rx) = sync_channel::<WriterJob>(QUEUE_CAPACITY);
    let worker_dead: WorkerDead = Arc::new(AtomicBool::new(false));

    if policy.storage.is_enabled() {
        let initial_line_count = if policy.max_entries_per_segment > 0 {
            seed_line_count(&policy.path)
        } else {
            0
        };
        let worker_state = Arc::new(WorkerState {
            policy: policy.clone(),
            worker_dead: Arc::clone(&worker_dead),
            line_count: AtomicU64::new(initial_line_count),
            next_archive_seq: AtomicU64::new(seed_archive_seq(&policy.path)),
        });
        spawn_worker(rx, worker_state);
    } else {
        // No worker will ever run for a disabled policy; mark dead so
        // flush_for_test / re-init waiters do not spin on a false flag.
        worker_dead.store(true, Ordering::Release);
        drop(rx);
    }

    let state = Arc::new(WriterState {
        policy,
        tx,
        worker_dead,
    });
    *slot().write() = Some(Arc::clone(&state));

    // Keep the handoff lock while replaying so no later producer can overtake
    // an event accepted during reload.
    let mut handoff = handoff_buffer().lock();
    let buffered = handoff.take().unwrap_or_default();
    for value in buffered {
        try_persist(&state, value);
    }
}

fn handoff_buffer() -> &'static parking_lot::Mutex<Option<Vec<Value>>> {
    HANDOFF_BUFFER.get_or_init(|| parking_lot::Mutex::new(None))
}

/// Remove the currently installed writer (if any), drain its queue, and wait
/// for the worker to sync and exit. The explicit shutdown job does not depend
/// on temporary `Arc<WriterState>` clones releasing their senders.
fn shutdown_current_writer(warn_after: Duration) {
    let previous = slot().write().take();
    let Some(prev) = previous else {
        return;
    };
    if prev.worker_dead.load(Ordering::Acquire) {
        return;
    }

    let (ack_tx, ack_rx) = sync_channel(0);
    let start = Instant::now();
    let mut shutdown = WriterJob::Shutdown(ack_tx);
    let mut warned_while_queued = false;
    loop {
        match prev.tx.try_send(shutdown) {
            Ok(()) => break,
            Err(TrySendError::Full(job)) if start.elapsed() < warn_after => {
                shutdown = job;
                thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Full(job)) => {
                tracing::warn!(
                    target: "zeroclaw_log",
                    "log: previous writer queue did not drain within {:?}; waiting to preserve exclusive ownership",
                    warn_after
                );
                warned_while_queued = true;
                if prev.tx.send(job).is_err() {
                    return;
                }
                break;
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
    drop(prev);

    if warned_while_queued {
        let _ = ack_rx.recv();
        return;
    }

    let remaining = warn_after.saturating_sub(start.elapsed());
    if ack_rx.recv_timeout(remaining).is_ok() {
        return;
    }

    tracing::warn!(
        target: "zeroclaw_log",
        "log: previous writer worker did not exit within {:?}; waiting to preserve exclusive ownership",
        warn_after
    );
    let _ = ack_rx.recv();
}

/// Spawn the disk-persistence worker thread. The worker owns the active
/// log file handle and processes `WriterJob`s until either the channel
/// closes (all senders dropped) or the process exits. On normal exit the
/// worker performs a final `sync_all` so any pending writes are durable.
fn spawn_worker(rx: Receiver<WriterJob>, state: Arc<WorkerState>) {
    let dead = Arc::clone(&state.worker_dead);
    let builder = thread::Builder::new().name("zeroclaw-log-writer".into());
    if let Err(err) = builder.spawn(move || worker_main(rx, state)) {
        tracing::warn!(
            target: "zeroclaw_log",
            error = %err,
            "log: failed to spawn zeroclaw-log-writer thread; persistence disabled"
        );
        dead.store(true, Ordering::Release);
    }
}

fn worker_main(rx: Receiver<WriterJob>, state: Arc<WorkerState>) {
    let mut writes_since_sync: u64 = 0;
    let mut last_sync = Instant::now();
    let mut shutdown_ack = None;

    loop {
        let job = match rx.recv_timeout(IDLE_TICK) {
            Ok(job) => Some(job),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if let Some(job) = job {
            match job {
                WriterJob::Write(value) => {
                    if let Err(err) = write_one(&state, &value) {
                        tracing::warn!(
                            target: "zeroclaw_log_internal",
                            error = ?err,
                            path = %state.policy.path.display(),
                            "log: worker write failed"
                        );
                    } else {
                        writes_since_sync += 1;
                    }
                }
                WriterJob::Flush(ack) => {
                    let result = sync_active_file(&state);
                    if let Err(err) = &result {
                        tracing::warn!(
                            target: "zeroclaw_log_internal",
                            error = ?err,
                            "log: worker flush sync_all failed"
                        );
                    }
                    writes_since_sync = 0;
                    last_sync = Instant::now();
                    let _ = ack.send(result);
                }
                WriterJob::Shutdown(ack) => {
                    shutdown_ack = Some(ack);
                    break;
                }
                #[cfg(test)]
                WriterJob::Pause { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
            }
        }

        if writes_since_sync > 0
            && (writes_since_sync >= SYNC_EVERY_N_WRITES || last_sync.elapsed() >= SYNC_INTERVAL)
        {
            if let Err(err) = sync_active_file(&state) {
                tracing::warn!(
                    target: "zeroclaw_log_internal",
                    error = ?err,
                    "log: worker periodic sync_all failed"
                );
            }
            writes_since_sync = 0;
            last_sync = Instant::now();
        }
    }

    // Channel closed or an ordered shutdown was requested. Final sync so any
    // writes pulled from the queue land on disk before exit is acknowledged.
    let _ = sync_active_file(&state);
    state.worker_dead.store(true, Ordering::Release);
    if let Some(ack) = shutdown_ack {
        let _ = ack.send(());
    }
}

/// Serialize + write one event. Opens the active file fresh, runs the
/// rotation hooks inline (so they can rename the file out from under
/// the next write), and drops the handle on return. No `sync_data` —
/// durability is the worker's periodic `sync_all`.
fn write_one(state: &Arc<WorkerState>, value: &Value) -> Result<()> {
    // Date-boundary rotation runs *before* the append so a new day's
    // first event lands in a fresh file. Idempotent when no rotation
    // is needed.
    if state.policy.storage == StoragePolicy::Rotating {
        if maybe_rotate_for_date(state)? {
            state.line_count.store(0, Ordering::Relaxed);
        }
        // The counter is seeded from the existing active file at startup, so
        // a file that already sits at (or above) the cap — an operator
        // enabling this mode on an existing log, or a reload from another
        // policy — must rotate *before* this append. Rotating only after the
        // append would leave `cap + 1` entries in the archive and break the
        // "maximum N entries per segment" contract. A freshly rotated file
        // has count 0, so this is a no-op on the steady-state path.
        if maybe_rotate_for_entry_count(state)? {
            state.line_count.store(0, Ordering::Relaxed);
        }
    }
    let mut file = open_active_file(state)?;
    {
        let mut writer = BufWriter::new(&mut file);
        write_jsonl_line(&mut writer, value)?;
        writer.flush()?;
    }
    match state.policy.storage {
        StoragePolicy::Rolling => trim_to_last_entries(state)?,
        StoragePolicy::Rotating => {
            state.line_count.fetch_add(1, Ordering::Relaxed);
            // Triggers are mutually exclusive per write: the first one that
            // fires rotates the file and resets the count, so a later
            // trigger in the same call never fires against a stale file.
            if maybe_rotate_for_size(state)? || maybe_rotate_for_entry_count(state)? {
                state.line_count.store(0, Ordering::Relaxed);
            }
        }
        StoragePolicy::None | StoragePolicy::Full => {}
    }
    Ok(())
}

/// Open or create the active file, then call `sync_all`. Used by explicit
/// flush, periodic sync after successful writes, and best-effort shutdown sync.
fn sync_active_file(state: &Arc<WorkerState>) -> Result<()> {
    let file = open_active_file(state)?;
    file.sync_all()
        .with_context(|| format!("syncing log file {}", state.policy.path.display()))?;
    Ok(())
}

/// Open (or create) the active log file in append mode with the
/// 0o600 Unix permission bit. Called once at worker startup and again
/// after any operation that may have renamed the file out from under
/// us (rotation, rolling trim).
fn open_active_file(state: &Arc<WorkerState>) -> Result<File> {
    if let Some(parent) = state.policy.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&state.policy.path)
        .with_context(|| format!("opening log file {}", state.policy.path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&state.policy.path, fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

/// Public accessor for the canonical log file path. Used by the gateway's
/// `/api/logs` endpoint to know which file to stream.
pub fn runtime_trace_path() -> Option<PathBuf> {
    current_state().map(|s| s.policy.path.clone())
}

/// The canonical *active* log file path: the path the currently installed
/// writer actually persists to, or `None` when persistence is disabled at the
/// writer layer. Unlike [`runtime_trace_path`], this is `None` when
/// `storage` is disabled, so consumers that gate the path on "is persistence
/// active?" read one source of truth (the writer state) instead of mixing a
/// runtime config snapshot with the writer state.
pub fn active_log_path() -> Option<PathBuf> {
    current_state()
        .filter(|s| s.policy.storage.is_enabled())
        .map(|s| s.policy.path.clone())
}

/// The active log path paired with whether the installed storage policy owns
/// timestamped archives. `None` when persistence is disabled at the writer
/// layer, matching [`active_log_path`].
///
/// Query entry points need both halves: the path says *where* to read, and the
/// flag says whether archives next to it are part of this policy's logical
/// stream. Reading the flag from the same writer state that produced the path
/// keeps the two consistent; deriving it from a separate config snapshot could
/// disagree with the writer actually running.
pub fn active_log_query_scope() -> Option<(PathBuf, bool)> {
    current_state()
        .filter(|s| s.policy.storage.is_enabled())
        .map(|s| (s.policy.path.clone(), s.policy.storage.reads_archives()))
}

pub fn flush_for_test() -> Result<()> {
    let Some(state) = current_state() else {
        return Ok(());
    };
    if !state.policy.storage.is_enabled() {
        return Ok(());
    }
    if state.worker_dead.load(Ordering::Acquire) {
        anyhow::bail!("log writer worker is not running");
    }
    let (ack_tx, ack_rx) = sync_channel(0);
    state
        .tx
        .send(WriterJob::Flush(ack_tx))
        .map_err(|_| anyhow::anyhow!("log writer worker disconnected before flush request"))?;
    ack_rx
        .recv()
        .context("log writer worker disconnected before reporting flush result")?
}

/// Resolved LLM-request-payload capture policy + the truncate cap, for the
/// turn engine's `announce_llm_request`. `None` when no writer is installed
/// (the policy defaults to `off`, so callers treat "no writer" as "off").
#[must_use]
pub fn llm_request_payload_policy() -> Option<(LlmRequestPayloadPolicy, usize)> {
    current_state().map(|s| {
        (
            s.policy.llm_request_payload,
            s.policy.tool_io_truncate_bytes,
        )
    })
}

/// Emit one event. Always fans out to the broadcast hook + tracing event.
/// If persistence is enabled, hands the serialized value to the disk
/// worker via a bounded `try_send`. The hot path performs no file I/O.
///
/// The broadcast copy carries `ephemeral_attributes` deep-merged into
/// `attributes` (live SSE consumers may render short-lived pairing
/// credentials); the persisted copy never does — `LogEvent` marks the
/// field `serde(skip)`, so the serialized value below is credential-free
/// by construction.
///
/// This is the function the `record!` macro expands into. Direct callers
/// (the schema migration tool, tests) can invoke it too, but production
/// code should go through the macro so the `tracing::event!` carries the
/// correct `file:line` source info.
pub fn record_event(event: LogEvent) {
    // `serde(skip)` on `ephemeral_attributes` keeps this value — the one
    // that reaches disk — free of broadcast-only secrets.
    let value = match serde_json::to_value(&event) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log_internal",
                error = ?err,
                "log: event serialization failed"
            );
            return;
        }
    };

    observer_bridge::forward(&event);

    if let Some(hook) = current_broadcast_hook() {
        let mut broadcast_value = value.clone();
        if !event.ephemeral_attributes.is_null() {
            merge_ephemeral_into_attributes(&mut broadcast_value, &event.ephemeral_attributes);
            // Mark the frame so a broadcast consumer that cannot authenticate
            // its subscribers (an unauthenticated `/api/events` stream) fails
            // closed on the pairing secret instead of fanning it out.
            if let Value::Object(map) = &mut broadcast_value {
                map.insert(EPHEMERAL_BROADCAST_MARKER.to_string(), Value::Bool(true));
            }
        }
        let _ = hook.send(broadcast_value);
    }

    // Serialize persistence handoff with re-init. Once reload begins, events
    // are buffered until the old worker has drained and the replacement is
    // installed, so migration cannot replace a file that is still receiving
    // accepted writes.
    let mut handoff = handoff_buffer().lock();
    if let Some(buffer) = handoff.as_mut() {
        if buffer.len() < QUEUE_CAPACITY {
            buffer.push(value);
        } else {
            tracing::warn!(
                target: "zeroclaw_log_internal",
                queue_capacity = QUEUE_CAPACITY,
                "log: reload handoff buffer full; dropping event"
            );
        }
        return;
    }
    let Some(state) = current_state() else {
        return;
    };
    try_persist(&state, value);
}

fn try_persist(state: &WriterState, value: Value) {
    if !state.policy.storage.is_enabled() {
        return;
    }

    let job = WriterJob::Write(value);
    match state.tx.try_send(job) {
        Ok(()) => {}
        Err(TrySendError::Full(dropped)) => {
            let _ = dropped;
            tracing::warn!(
                target: "zeroclaw_log_internal",
                path = %state.policy.path.display(),
                queue_capacity = QUEUE_CAPACITY,
                "log: writer queue full; dropping event"
            );
        }
        Err(TrySendError::Disconnected(_)) => {
            state.worker_dead.store(true, Ordering::Release);
        }
    }
}

/// True when a broadcast frame was stamped with [`EPHEMERAL_BROADCAST_MARKER`]
/// by [`record_event`] — i.e. it carries broadcast-only pairing secrets (QR
/// payloads, pair codes) deep-merged into `attributes`.
///
/// Every consumer of the shared broadcast bus (the gateway SSE stream, the RPC
/// `logs/subscribe` forwarder) uses this to fail closed on the credential
/// unless it can prove its subscriber is authenticated.
pub fn frame_carries_ephemeral_credentials(value: &Value) -> bool {
    value
        .get(EPHEMERAL_BROADCAST_MARKER)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Strip the internal [`EPHEMERAL_BROADCAST_MARKER`] from a broadcast frame
/// before it is delivered to a consumer, so the public event shape is
/// unchanged. Returns whether the marker was present.
pub fn strip_ephemeral_broadcast_marker(value: &mut Value) -> bool {
    value
        .as_object_mut()
        .and_then(|obj| obj.remove(EPHEMERAL_BROADCAST_MARKER))
        .is_some()
}

/// Deep-merge the event's `ephemeral_attributes` into the broadcast
/// value's `attributes` object. Ephemeral keys win on conflict (they are
/// the fresher, call-site-provided data). Only object-into-object merges
/// recurse; any other shape replaces wholesale.
fn merge_ephemeral_into_attributes(broadcast_value: &mut Value, ephemeral: &Value) {
    let Some(root) = broadcast_value.as_object_mut() else {
        return;
    };
    let attributes = root
        .entry("attributes".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    deep_merge(attributes, ephemeral);
}

fn deep_merge(target: &mut Value, incoming: &Value) {
    match (target, incoming) {
        (Value::Object(target_map), Value::Object(incoming_map)) => {
            for (key, incoming_child) in incoming_map {
                match target_map.get_mut(key) {
                    Some(target_child) => deep_merge(target_child, incoming_child),
                    None => {
                        target_map.insert(key.clone(), incoming_child.clone());
                    }
                }
            }
        }
        (target_slot, incoming_value) => {
            *target_slot = incoming_value.clone();
        }
    }
}

/// Serialize one event as a single JSONL line (terminated with `\n`) to the
/// provided buffered writer. Pure helper: does not open, flush, or fsync the
/// file — the caller owns the [`BufWriter`] lifecycle.
///
/// Used by the production append path (`append_line`). The rolling trim path
/// (`trim_to_last_entries`) writes the original JSONL bytes from the
/// line-buffered reader directly, so it stays inline rather than going
/// through this helper (re-serializing would risk non-byte-identical output
/// for non-canonical input, e.g. reordered keys or whitespace).
fn write_jsonl_line<W: Write + ?Sized>(writer: &mut W, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value).context("serializing log line")?;
    writer.write_all(b"\n").context("writing newline")?;
    Ok(())
}

/// Rolling trim. Streams the file line-by-line into a temp file, keeping
/// the last `max_entries` lines, then atomically renames. Never loads the
/// whole file into memory.
fn trim_to_last_entries(state: &Arc<WorkerState>) -> Result<()> {
    // Count lines first (cheap pass).
    let total = count_nonempty_lines(&state.policy.path)?;
    if total <= state.policy.max_entries {
        return Ok(());
    }
    let skip = total - state.policy.max_entries;

    let tmp = state.policy.path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
    ));

    {
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let out_file = opts
            .open(&tmp)
            .with_context(|| format!("creating trim temp file {}", tmp.display()))?;
        let mut out = BufWriter::new(out_file);

        let in_file = fs::File::open(&state.policy.path)
            .with_context(|| format!("opening log for trim: {}", state.policy.path.display()))?;
        let reader = BufReader::new(in_file);

        let mut index: usize = 0;
        for line in reader.lines() {
            let line = line.context("reading log line during trim")?;
            if line.trim().is_empty() {
                continue;
            }
            if index >= skip {
                out.write_all(line.as_bytes())
                    .context("writing trim line")?;
                out.write_all(b"\n").context("writing trim newline")?;
            }
            index += 1;
        }
        out.flush().context("flushing trim file")?;
        out.into_inner()
            .context("taking trim file out of buf writer")?
            .sync_data()
            .context("fsync trim file")?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &state.policy.path).with_context(|| {
        format!(
            "renaming trim temp {} → {}",
            tmp.display(),
            state.policy.path.display()
        )
    })?;

    Ok(())
}

fn count_nonempty_lines(path: &Path) -> Result<usize> {
    let file = fs::File::open(path)
        .with_context(|| format!("opening log to count lines: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut n = 0usize;
    for line in reader.lines() {
        let line = line.context("reading log line for count")?;
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

/// Seed the in-memory line counter at worker startup. A missing active file
/// is the normal state for a fresh workspace and returns `0` silently.
/// Any other I/O error is logged at `warn` level and also returns `0` so the
/// worker can start rather than panicking; it will self-correct once enough
/// events are written to trigger an entry-count rotation.
fn seed_line_count(path: &Path) -> u64 {
    match count_nonempty_lines(path) {
        Ok(n) => n as u64,
        Err(ref err)
            if err
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(|e| e.kind() == std::io::ErrorKind::NotFound)
                .unwrap_or(false) =>
        {
            0
        }
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log",
                error = ?err,
                "log: could not seed entry-count from active file; starting from 0"
            );
            0
        }
    }
}

/// Rotate the active file to an archive when it has crossed a UTC day boundary
/// since its last write. No-op when daily rotation is off, the file is absent,
/// or it was last written today. Returns `true` when a rotation occurred.
fn maybe_rotate_for_date(state: &Arc<WorkerState>) -> Result<bool> {
    if !state.policy.rotate_daily {
        return Ok(false);
    }
    let path = &state.policy.path;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat log for date rotation: {}", path.display()));
        }
    };
    // An empty active file has nothing worth archiving.
    if meta.len() == 0 {
        return Ok(false);
    }
    let modified: DateTime<Utc> = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
    if modified.date_naive() < Utc::now().date_naive() {
        rotate_active(state, modified)?;
        return Ok(true);
    }
    Ok(false)
}

/// Rotate the active file when a just-completed append left it at or above the
/// configured byte budget. No-op when size rotation is disabled (`max_bytes`
/// `== 0`) or the file is under budget. Returns `true` when a rotation occurred.
fn maybe_rotate_for_size(state: &Arc<WorkerState>) -> Result<bool> {
    let max = state.policy.max_bytes;
    if max == 0 {
        return Ok(false);
    }
    let path = &state.policy.path;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat log for size rotation: {}", path.display()));
        }
    };
    if meta.len() >= max {
        // Stamp the archive with the file's last-write time (its newest event),
        // matching date rotation, so the archive name reflects its contents.
        let when: DateTime<Utc> = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
        rotate_active(state, when)?;
        return Ok(true);
    }
    Ok(false)
}

/// Rotate the active file when the in-memory line counter says the segment has
/// reached the configured entry cap. Returns `true` when a rotation occurred.
fn maybe_rotate_for_entry_count(state: &Arc<WorkerState>) -> Result<bool> {
    let cap = state.policy.max_entries_per_segment;
    if cap == 0 {
        return Ok(false);
    }
    let count = state.line_count.load(Ordering::Relaxed);
    if count < cap as u64 {
        return Ok(false);
    }
    let path = &state.policy.path;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat log for entry-count rotation: {}", path.display()));
        }
    };
    if meta.len() == 0 {
        return Ok(false);
    }
    let when: DateTime<Utc> = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
    rotate_active(state, when)?;
    Ok(true)
}

fn rotate_active(state: &Arc<WorkerState>, when: DateTime<Utc>) -> Result<()> {
    let path = &state.policy.path;
    // Claim the next number before renaming so the archive name records where
    // this segment falls in the series.
    let seq = state.next_archive_seq.fetch_add(1, Ordering::Relaxed);
    let archive = archive_path(path, when, seq)?;
    fs::rename(path, &archive)
        .with_context(|| format!("rotating log {} → {}", path.display(), archive.display()))?;

    // Record the number before retention runs: retention may delete this very
    // archive, and the series must not fall back to it on the next restart.
    record_seq_watermark(path, seq);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&archive, fs::Permissions::from_mode(0o600));
    }

    run_retention(&state.policy);
    Ok(())
}

/// Path of the sidecar that records the highest archive sequence ever issued.
///
/// Sits next to the active log as `<basename>.seq`, which does not match the
/// archive-name shape, so `list_archives` never mistakes it for a segment.
fn seq_watermark_path(active: &Path) -> PathBuf {
    let dir = active.parent().unwrap_or_else(|| Path::new("."));
    let name = active
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("runtime-trace");
    dir.join(format!("{name}.seq"))
}

/// Record `seq` as the highest number ever issued, if it beats what is stored.
///
/// Best-effort: a failure here costs monotonicity across a wipe-and-restart,
/// which the reader already handles as an unresolvable cursor, so it must never
/// fail the append that triggered the rotation.
fn record_seq_watermark(active: &Path, seq: u64) {
    let path = seq_watermark_path(active);
    let stored = read_seq_watermark(active);
    if stored >= seq {
        return;
    }
    if let Err(err) = fs::write(&path, seq.to_string()) {
        tracing::warn!(
            target: "zeroclaw_log_internal",
            error = ?err,
            path = %path.display(),
            "log: could not record the rotation sequence high-water mark; a later \
             restart may reuse a number if retention removes every archive",
        );
    }
}

/// Highest sequence recorded by a previous run, or `0` when there is none.
fn read_seq_watermark(active: &Path) -> u64 {
    fs::read_to_string(seq_watermark_path(active))
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

/// The next archive sequence number to issue.
///
/// Taken from two sources, whichever is higher: the numbered archives still on
/// disk, and a small sidecar recording the highest number ever issued. The
/// archives alone are not enough. Age-based retention can delete every numbered
/// archive, and a restart would then begin again at 1 while a client still
/// holds a cursor naming sequence 1. That cursor is supposed to be
/// unresolvable, but it would instead bind to an unrelated future archive and
/// show the client history it never asked for. The sidecar keeps the series
/// monotonic across that wipe.
///
/// Legacy archives written before numbering existed contribute nothing here.
/// They sort before every numbered archive, so starting a fresh series at 1
/// alongside them is correct.
fn seed_archive_seq(active: &Path) -> u64 {
    let on_disk = match list_archives(active) {
        Ok(archives) => archives
            .iter()
            .filter_map(|(_, order)| match order {
                ArchiveOrder::Seq(n) => Some(*n),
                ArchiveOrder::Legacy(_) => None,
            })
            .max()
            .unwrap_or(0),
        Err(err) => {
            // A directory that cannot be listed is reported by the query path
            // as well. The watermark below still bounds the series, so this
            // only loses the archives-on-disk half of the comparison.
            tracing::warn!(
                target: "zeroclaw_log",
                error = ?err,
                path = %active.display(),
                "log: could not read existing archives to seed the rotation sequence",
            );
            0
        }
    };
    on_disk.max(read_seq_watermark(active)).saturating_add(1)
}

/// Build the archive path for `path`, stamping the timestamp before the
/// extension and disambiguating same-second rotations with a numeric suffix.
fn archive_path(path: &Path, when: DateTime<Utc>, seq: u64) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("log path has no file name")?;
    let (base, ext) = split_base_ext(file_name);
    let stamp = when.format("%Y%m%d-%H%M%S").to_string();

    // The sequence number leads so the name itself carries segment order: a
    // reader sorts by it and never has to infer order from mtimes, which are
    // an observation made at enumeration time and can misorder segments when
    // several rotations land during one read. The stamp stays for operators
    // reading the directory.
    //
    // The existence check and the subsequent `fs::rename` both run on the
    // single-threaded worker, so this check-then-rename has no in-process
    // race. A collision here means the seeded series overlapped something on
    // disk; bumping the sequence keeps names unique and order intact.
    let mut seq = seq;
    let width = SEQ_WIDTH;
    let mut candidate = dir.join(format!("{base}.{seq:0width$}-{stamp}{ext}"));
    while candidate.exists() {
        seq = seq.saturating_add(1);
        candidate = dir.join(format!("{base}.{seq:0width$}-{stamp}{ext}"));
    }
    Ok(candidate)
}

/// Split a log file name into `(base, ext)` where `ext` includes the leading
/// dot (or is empty). The split is on the *last* dot so multi-dot names keep
/// only their final extension: `runtime-trace.jsonl` → `("runtime-trace",
/// ".jsonl")`; `a.b.jsonl` → `("a.b", ".jsonl")`; `trace` → `("trace", "")`.
/// Prune rotated archives by age then by count. Best-effort: a removal failure
/// is logged but never fails the enclosing append, since retention is
/// housekeeping rather than part of the durability contract.
fn run_retention(policy: &ResolvedPolicy) {
    let max_files = policy.retention_max_files;
    let max_age_days = policy.retention_max_age_days;
    if max_files == 0 && max_age_days == 0 {
        return;
    }

    let mut archives = match list_archives(&policy.path) {
        Ok(a) => a,
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log_internal",
                error = ?err,
                "log: listing archives for retention failed",
            );
            return;
        }
    };
    // Newest first, so a later count cap keeps the most recent archives. The
    // ordering key comes from the archive name where available, so it stays
    // correct even if a file's mtime was touched after it was written.
    archives.sort_by_key(|(_, order)| std::cmp::Reverse(*order));

    // Age-based cleanup. This one genuinely needs wall-clock age, so it reads
    // mtime from the filesystem rather than the name-derived ordering key. An
    // archive whose mtime cannot be read is kept: deleting a file we failed to
    // measure would be the more destructive choice.
    if max_age_days > 0
        && let Some(cutoff) =
            SystemTime::now().checked_sub(Duration::from_secs(max_age_days.saturating_mul(86_400)))
    {
        archives.retain(|(p, _)| {
            let Ok(mtime) = fs::metadata(p).and_then(|m| m.modified()) else {
                return true;
            };
            if mtime < cutoff {
                remove_archive(p);
                false
            } else {
                true
            }
        });
    }

    // Count-based cleanup: keep the newest `max_files`, drop the rest.
    if max_files > 0 && archives.len() > max_files {
        for (p, _) in archives.iter().skip(max_files) {
            remove_archive(p);
        }
    }
}

fn remove_archive(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        tracing::warn!(
            target: "zeroclaw_log_internal",
            error = ?err,
            path = %path.display(),
            "log: pruning rotated archive failed",
        );
    }
}

pub(crate) static WRITER_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, Severity};
    // Archive-name predicates live in `reader`; only the tests below exercise
    // them directly, so importing them at module scope would be unused in a
    // non-test build.
    use crate::reader::{is_archive_core, is_stamp};

    fn install_writer(dir: &Path, max_entries: usize) {
        let cfg = LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: max_entries,
            ..LogConfig::default()
        };
        init_from_config(&cfg, dir);
    }

    fn detached_enabled_policy(dir: &Path) -> ResolvedPolicy {
        install_writer(dir, 10);
        let policy = current_state().unwrap().policy.clone();
        shutdown_current_writer(SHUTDOWN_WARN_AFTER);
        policy
    }

    fn install_test_state(
        policy: ResolvedPolicy,
        tx: SyncSender<WriterJob>,
        worker_dead: WorkerDead,
    ) {
        *slot().write() = Some(Arc::new(WriterState {
            policy,
            tx,
            worker_dead,
        }));
    }

    #[test]
    fn flush_without_writer_is_noop() {
        let _guard = WRITER_TEST_LOCK.lock();
        shutdown_current_writer(SHUTDOWN_WARN_AFTER);

        flush_for_test().unwrap();
    }

    #[test]
    fn flush_with_disabled_storage_is_noop() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            log_persistence: "none".into(),
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        flush_for_test().unwrap();
    }

    #[test]
    fn flush_before_first_write_creates_empty_active_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 10);

        flush_for_test().unwrap();

        let path = runtime_trace_path().unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "");
    }

    #[test]
    fn flush_reports_active_file_open_failure() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 10);

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(&path).unwrap();

        let err = flush_for_test().unwrap_err();
        assert!(
            format!("{err:#}").contains("opening log file"),
            "flush should report the active-file open failure: {err:#}"
        );
    }

    #[test]
    fn flush_reports_dead_worker() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let policy = detached_enabled_policy(tmp.path());
        let (tx, rx) = sync_channel(1);
        drop(rx);
        install_test_state(policy, tx, Arc::new(AtomicBool::new(true)));

        let err = flush_for_test().unwrap_err();
        slot().write().take();
        assert_eq!(err.to_string(), "log writer worker is not running");
    }

    #[test]
    fn flush_reports_request_channel_disconnect() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let policy = detached_enabled_policy(tmp.path());
        let (tx, rx) = sync_channel(1);
        drop(rx);
        install_test_state(policy, tx, Arc::new(AtomicBool::new(false)));

        let err = flush_for_test().unwrap_err();
        slot().write().take();
        assert_eq!(
            err.to_string(),
            "log writer worker disconnected before flush request"
        );
    }

    #[test]
    fn flush_reports_result_channel_disconnect() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let policy = detached_enabled_policy(tmp.path());
        let (tx, rx) = sync_channel(1);
        install_test_state(policy, tx, Arc::new(AtomicBool::new(false)));
        let worker = thread::spawn(move || match rx.recv().unwrap() {
            WriterJob::Flush(ack) => drop(ack),
            _ => panic!("expected flush job"),
        });

        let err = flush_for_test().unwrap_err();
        worker.join().unwrap();
        slot().write().take();
        assert_eq!(
            err.to_string(),
            "log writer worker disconnected before reporting flush result"
        );
    }

    #[test]
    fn append_and_rolling_keeps_only_max_entries() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 3);

        for i in 0..10 {
            let mut ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
            ev.message = Some(format!("event-{i}"));
            record_event(ev);
        }

        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3);
        // Last three should be 7, 8, 9 (oldest to newest order preserved).
        for (idx, &line) in lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["message"].as_str().unwrap(), format!("event-{}", idx + 7));
        }
    }

    #[test]
    fn ephemeral_attributes_reach_broadcast_but_never_disk() {
        let _guard = WRITER_TEST_LOCK.lock();
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 10);

        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        crate::broadcast::set_broadcast_hook(tx);

        let mut ev = LogEvent::new(Severity::Info, "test", EventCategory::Channel);
        ev.message = Some("qr ready".to_string());
        ev.attributes = serde_json::json!({
            "login": { "state": "qr", "channel": "wechat.assistant" }
        });
        ev.ephemeral_attributes = serde_json::json!({
            "login": { "qr_payload": "SECRET-QR-PAYLOAD" }
        });
        record_event(ev);

        // Broadcast copy: ephemeral fields deep-merged into attributes.
        let broadcast = rx.try_recv().expect("broadcast copy delivered");
        assert_eq!(
            broadcast["attributes"]["login"]["qr_payload"],
            "SECRET-QR-PAYLOAD"
        );
        assert_eq!(broadcast["attributes"]["login"]["state"], "qr");

        // Persisted copy: the credential never lands on disk.
        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("SECRET-QR-PAYLOAD"),
            "persisted JSONL must not contain ephemeral credentials: {contents}"
        );
        assert!(
            contents.contains("\"state\":\"qr\""),
            "persisted JSONL keeps the lifecycle state: {contents}"
        );
        let line: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert!(line["attributes"]["login"].get("qr_payload").is_none());
        assert!(line.get("ephemeral_attributes").is_none());
        // The broadcast-only fail-closed marker also never reaches disk.
        assert!(line.get(EPHEMERAL_BROADCAST_MARKER).is_none());

        crate::broadcast::clear_broadcast_hook();
    }

    /// A frame carrying `ephemeral_attributes` is stamped with the
    /// fail-closed marker on the broadcast copy so an SSE layer that cannot
    /// authenticate its subscribers can withhold the pairing secret. A frame
    /// with no ephemeral attributes is never stamped.
    #[test]
    fn broadcast_frame_marks_ephemeral_credentials_for_fail_closed_delivery() {
        let _guard = WRITER_TEST_LOCK.lock();
        let _hook_guard = crate::broadcast::HOOK_TEST_LOCK.lock();

        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        crate::broadcast::set_broadcast_hook(tx);

        let mut with_secret = LogEvent::new(Severity::Info, "test", EventCategory::Channel);
        with_secret.attributes = serde_json::json!({ "login": { "state": "qr" } });
        with_secret.ephemeral_attributes =
            serde_json::json!({ "login": { "qr_payload": "SECRET-QR-PAYLOAD" } });
        record_event(with_secret);
        let framed = rx.try_recv().expect("broadcast copy delivered");
        assert_eq!(
            framed[EPHEMERAL_BROADCAST_MARKER], true,
            "credential-bearing frame must be marked for fail-closed delivery: {framed}"
        );

        let mut no_secret = LogEvent::new(Severity::Info, "test", EventCategory::Channel);
        no_secret.attributes = serde_json::json!({ "login": { "state": "connected" } });
        record_event(no_secret);
        let plain = rx.try_recv().expect("broadcast copy delivered");
        assert!(
            plain.get(EPHEMERAL_BROADCAST_MARKER).is_none(),
            "credential-free frame must not be marked: {plain}"
        );

        crate::broadcast::clear_broadcast_hook();
    }

    #[test]
    fn deep_merge_prefers_ephemeral_on_conflict_and_recurses() {
        let mut target = serde_json::json!({
            "login": { "state": "qr", "attempt": 1 },
            "other": "kept"
        });
        let incoming = serde_json::json!({
            "login": { "qr_payload": "p", "attempt": 2 }
        });
        deep_merge(&mut target, &incoming);
        assert_eq!(target["login"]["state"], "qr");
        assert_eq!(target["login"]["qr_payload"], "p");
        assert_eq!(target["login"]["attempt"], 2);
        assert_eq!(target["other"], "kept");
    }

    #[test]
    fn disabled_storage_does_not_write_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            log_persistence: "none".into(),
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        let event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        record_event(event);

        let path = runtime_trace_path().unwrap();
        assert!(
            !path.exists(),
            "no file should exist when storage is disabled"
        );
    }

    #[test]
    fn reinit_applies_changed_rotation_policy() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, false, 0, 0);

        emit("before-reload");
        flush_for_test().unwrap();

        let path = runtime_trace_path().unwrap();
        assert!(
            list_archives(&path).unwrap().is_empty(),
            "size rotation should be disabled before re-init"
        );
        assert_eq!(total_events(&path), 1);

        install_rotating(tmp.path(), 1, false, 1, 0);
        emit("after-reload");
        flush_for_test().unwrap();

        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "re-init should apply the new byte budget and rotate on the next append"
        );
        assert_eq!(
            total_events(&path),
            2,
            "re-init must preserve already-written events while applying the new policy"
        );
    }

    #[test]
    fn reinit_can_disable_persistence_without_restart() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 10);

        emit("persisted-before-disable");
        flush_for_test().unwrap();

        let path = runtime_trace_path().unwrap();
        assert_eq!(count_lines(&path), 1);

        let cfg = LogConfig {
            log_persistence: "none".into(),
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        emit("not-persisted-after-disable");
        // Disabled policy has no worker; flush is a no-op. Prior writes were
        // drained by the shutdown path inside init_from_config.
        flush_for_test().unwrap();

        assert_eq!(
            count_lines(&path),
            1,
            "disabled persistence after re-init should stop appending without deleting existing logs"
        );
    }

    #[test]
    fn reinit_shuts_down_previous_worker_before_installing_new() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 10);

        let first = current_state().expect("writer installed");
        let first_dead = Arc::clone(&first.worker_dead);
        assert!(
            !first_dead.load(Ordering::Acquire),
            "fresh worker should be alive"
        );
        // Re-init with a different policy must tear down the first worker
        // before installing the second, even while this test holds a state
        // clone (and therefore another sender) across the handoff.
        install_rotating(tmp.path(), 0, false, 0, 0);

        assert!(
            first_dead.load(Ordering::Acquire),
            "previous worker must exit during re-init so it cannot race the new policy"
        );
        let second = current_state().expect("replacement writer installed");
        assert!(
            !second.worker_dead.load(Ordering::Acquire),
            "replacement worker should be alive"
        );
        // The replacement must not share the previous worker_dead flag —
        // that would mean the old Arc was reused rather than replaced.
        assert!(
            !Arc::ptr_eq(&first_dead, &second.worker_dead),
            "re-init must install a fresh WriterState, not mutate the old one"
        );
    }

    #[test]
    fn reinit_waits_past_shutdown_warning_before_replacing_writer() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 100);

        let first = current_state().expect("writer installed");
        let (entered_tx, entered_rx) = sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        first
            .tx
            .send(WriterJob::Pause {
                entered: entered_tx,
                release: release_rx,
            })
            .unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("old worker should enter the deterministic pause");

        let mut late_old = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        late_old.message = Some("old-worker-after-warning".to_string());
        first
            .tx
            .send(WriterJob::Write(serde_json::to_value(late_old).unwrap()))
            .unwrap();
        drop(first);

        let cfg = LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: 100,
            ..LogConfig::default()
        };
        let dir = tmp.path().to_path_buf();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reinit = thread::spawn(move || {
            init_from_config_with_migration_and_shutdown_warning(
                &cfg,
                &dir,
                |_| Ok(()),
                Duration::from_millis(20),
            );
            done_tx.send(()).unwrap();
        });

        let handoff_start = Instant::now();
        while handoff_buffer().lock().is_none() {
            assert!(
                handoff_start.elapsed() < Duration::from_secs(1),
                "re-init should activate the handoff buffer"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            done_rx.recv_timeout(Duration::from_millis(60)).is_err(),
            "re-init must not install a replacement after only reaching the warning threshold"
        );
        let mut accepted = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        accepted.message = Some("accepted-during-timeout-handoff".to_string());
        record_event(accepted);

        release_tx.send(()).unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("re-init should finish after the old worker resumes and exits");
        reinit.join().unwrap();
        flush_for_test().unwrap();

        let path = runtime_trace_path().unwrap();
        let messages: Vec<String> = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter_map(|event| {
                event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message == "old-worker-after-warning")
        );
        assert!(
            messages
                .iter()
                .any(|message| message == "accepted-during-timeout-handoff"),
            "an event accepted past the shutdown warning must survive after exclusive ownership transfers"
        );
    }

    #[test]
    fn reinit_preserves_event_accepted_while_migrating() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 100);
        emit("before-reload");

        let path = runtime_trace_path().unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            r#"{{"id":"legacy-id","timestamp":"2026-05-15T19:00:00Z","event_type":"legacy-test","message":"legacy-row"}}"#
        )
        .unwrap();
        file.sync_all().unwrap();

        let cfg = LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: 100,
            ..LogConfig::default()
        };
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let producer = thread::spawn(move || {
            start_rx.recv().unwrap();
            let mut event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
            event.message = Some("accepted-during-migration".to_string());
            record_event(event);
            accepted_tx.send(()).unwrap();
        });

        init_from_config_with_migration(&cfg, tmp.path(), |migration_path| {
            start_tx.send(()).unwrap();
            accepted_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("concurrent event should be accepted into the handoff buffer");
            migrate::migrate_legacy_jsonl_in_place(migration_path)
        });
        producer.join().unwrap();
        flush_for_test().unwrap();

        let events: Vec<Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            events.iter().any(|event| {
                event.get("message").and_then(Value::as_str) == Some("accepted-during-migration")
            }),
            "an event accepted during migration must be replayed to the replacement writer"
        );
        assert!(
            events.iter().any(|event| {
                event.get("id").and_then(Value::as_str) == Some("legacy-id")
                    && event.get("schema_version").and_then(Value::as_u64) == Some(2)
            }),
            "the legacy row should still be migrated during the quiescent handoff"
        );
    }

    #[test]
    fn migration_makes_unterminated_tail_safe_for_first_writer_append() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            log_persistence: "full".into(),
            ..LogConfig::default()
        };
        let path = ResolvedPolicy::from_config(&cfg, tmp.path()).path;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"id":"legacy-id","timestamp":"2026-05-15T19:00:00Z","event_type":"legacy-test","message":"legacy-row"}"#,
        )
        .unwrap();

        init_from_config(&cfg, tmp.path());
        let mut event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        event.message = Some("first-event-after-migration".to_string());
        record_event(event);
        flush_for_test().unwrap();

        let lines: Vec<Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(lines.iter().any(|line| {
            line["id"] == "legacy-id" && line["schema_version"] == LogEvent::SCHEMA_VERSION
        }));
        assert!(
            lines
                .iter()
                .any(|line| line["message"] == "first-event-after-migration")
        );
    }

    // ── Rotation (StoragePolicy::Rotating) ───────────────────────

    fn install_rotating(
        dir: &Path,
        max_bytes: u64,
        rotate_daily: bool,
        max_files: usize,
        max_age_days: u64,
    ) {
        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_max_bytes: max_bytes,
            log_persistence_rotate_daily: rotate_daily,
            log_persistence_retention_max_files: max_files,
            log_persistence_retention_max_age_days: max_age_days,
            ..LogConfig::default()
        };
        init_from_config(&cfg, dir);
    }

    fn emit(msg: &str) {
        let mut ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        ev.message = Some(msg.to_string());
        record_event(ev);
        // JSONL fsync runs off the async hot path, so `record_event` returns
        // before the line is on disk. Tests that assert on the log file
        // immediately after emitting must flush first (the explicit idiom used
        // throughout this module); folding it into `emit` keeps every
        // emit-then-read test correct — notably the `reinit_*` tests, which
        // read the file right after emitting.
        flush_for_test().unwrap();
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn count_lines(path: &Path) -> usize {
        fs::read_to_string(path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    /// Total events preserved across the active file plus every archive.
    fn total_events(active: &Path) -> usize {
        let mut n = count_lines(active);
        for (p, _) in list_archives(active).unwrap() {
            n += count_lines(&p);
        }
        n
    }

    #[test]
    fn rotating_size_triggers_archive_without_data_loss() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Tiny byte budget; daily off so only size drives rotation.
        install_rotating(tmp.path(), 200, false, 0, 0);

        for i in 0..20 {
            emit(&format!("event-{i}"));
        }

        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        let archives = list_archives(&path).unwrap();
        assert!(
            !archives.is_empty(),
            "size rotation should have produced at least one archive"
        );
        // Rotation archives rather than discards: every event is still on disk.
        assert_eq!(
            total_events(&path),
            20,
            "no events should be lost across rotation"
        );
    }

    #[test]
    fn rotating_daily_archives_previous_day_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, true, 0, 0); // daily only

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed an active file last written two days ago.
        fs::write(&path, "{\"message\":\"yesterday\"}\n").unwrap();
        set_mtime(&path, SystemTime::now() - Duration::from_secs(2 * 86_400));

        // Today's first event must archive the stale file and start fresh.
        emit("today");

        flush_for_test().unwrap();
        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "the previous-day file should be archived exactly once"
        );
        let archive_body = fs::read_to_string(&archives[0].0).unwrap();
        assert!(archive_body.contains("yesterday"));
        assert!(!archive_body.contains("today"));
        let active_body = fs::read_to_string(&path).unwrap();
        assert!(active_body.contains("today"));
        assert!(!active_body.contains("yesterday"));
    }

    #[test]
    fn rotating_without_triggers_keeps_all_in_active_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // No size budget and no daily boundary: behaves like `full`.
        install_rotating(tmp.path(), 0, false, 0, 0);

        for i in 0..15 {
            emit(&format!("e-{i}"));
        }

        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        assert!(
            list_archives(&path).unwrap().is_empty(),
            "no rotation should occur when both triggers are disabled"
        );
        assert_eq!(total_events(&path), 15);
    }

    #[test]
    fn full_mode_persists_all_without_trim_or_rotation() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Backwards-compat: `full` ignores max_entries and never rotates.
        let cfg = LogConfig {
            log_persistence: "full".into(),
            log_persistence_max_entries: 2,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        for i in 0..6 {
            emit(&format!("f-{i}"));
        }

        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        assert_eq!(total_events(&path), 6, "full keeps every event");
        assert!(list_archives(&path).unwrap().is_empty());
    }

    #[test]
    fn retention_prunes_oldest_archives_by_count() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, false, 2, 0); // keep newest 2

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();
        let base = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let mut archives = Vec::new();
        for i in 0..4u64 {
            let p = dir.join(format!("runtime-trace.2026010{i}-000000.jsonl"));
            fs::write(&p, "{}\n").unwrap();
            set_mtime(&p, base + Duration::from_secs(i * 3_600));
            archives.push(p);
        }

        run_retention(&current_state().unwrap().policy);

        assert!(
            !archives[0].exists() && !archives[1].exists(),
            "the two oldest archives should be pruned"
        );
        assert!(
            archives[2].exists() && archives[3].exists(),
            "the two newest archives should be kept"
        );
    }

    #[test]
    fn retention_prunes_archives_by_age() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, false, 0, 1); // keep <= 1 day, no count cap

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();
        let old = dir.join("runtime-trace.20260101-000000.jsonl");
        let recent = dir.join("runtime-trace.20260109-000000.jsonl");
        fs::write(&old, "{}\n").unwrap();
        fs::write(&recent, "{}\n").unwrap();
        set_mtime(&old, SystemTime::now() - Duration::from_secs(3 * 86_400));
        set_mtime(&recent, SystemTime::now() - Duration::from_secs(3_600));

        run_retention(&current_state().unwrap().policy);

        assert!(!old.exists(), "archive older than the age cap is pruned");
        assert!(recent.exists(), "recent archive is kept");
    }

    #[test]
    fn archive_path_leads_with_the_sequence_and_dedupes_on_collision() {
        use chrono::TimeZone;
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("runtime-trace.jsonl");
        let when = Utc.with_ymd_and_hms(2026, 6, 24, 3, 15, 0).unwrap();

        // The sequence leads the name so readers can order segments by name
        // alone; the stamp stays for operators reading the directory.
        let a1 = archive_path(&active, when, 1).unwrap();
        assert_eq!(
            a1.file_name().unwrap().to_str().unwrap(),
            "runtime-trace.0000000001-20260624-031500.jsonl"
        );

        // A number already taken on disk is skipped rather than reused, so two
        // archives never share an ordering key.
        fs::write(&a1, "x").unwrap();
        let a2 = archive_path(&active, when, 1).unwrap();
        assert_eq!(
            a2.file_name().unwrap().to_str().unwrap(),
            "runtime-trace.0000000002-20260624-031500.jsonl"
        );
    }

    #[test]
    fn archive_sequence_never_reuses_a_number_after_retention_wipes_every_archive() {
        // The cursor contract says an archive sequence is never reused, so a
        // cursor naming a pruned segment stays unresolvable and the reader can
        // honestly report the end of history. Deriving the next number only
        // from the archives still on disk breaks that at one boundary: age
        // retention can delete every numbered archive, and a restart would then
        // begin again at 1. A client still holding `1:<off>` would have it bind
        // to an unrelated future archive and be shown history it never asked
        // for. A sidecar records the highest number ever issued so the series
        // survives that wipe.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("runtime-trace.jsonl");
        fs::write(&active, "").unwrap();

        // Rotations reached sequence 5, then retention removed every archive.
        record_seq_watermark(&active, 5);
        assert!(
            list_archives(&active)
                .unwrap()
                .iter()
                .all(|(_, order)| !matches!(order, ArchiveOrder::Seq(_))),
            "test setup: no numbered archive may remain on disk"
        );

        assert_eq!(
            seed_archive_seq(&active),
            6,
            "the series must resume past every number it has already issued, \
             even with no archive left to read it from"
        );
    }

    #[test]
    fn archive_sequence_prefers_whichever_source_is_higher() {
        // Both sources bound the series: the archives still on disk and the
        // recorded high-water mark. Whichever is higher wins, so neither a
        // stale sidecar nor a wiped directory can pull the series backwards.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("runtime-trace.jsonl");
        fs::write(&active, "").unwrap();
        fs::write(
            tmp.path()
                .join("runtime-trace.0000000012-20260624-031500.jsonl"),
            "x\n",
        )
        .unwrap();

        // Sidecar behind the archives: the archive on disk decides.
        record_seq_watermark(&active, 4);
        assert_eq!(seed_archive_seq(&active), 13);

        // Sidecar ahead of them: the watermark decides.
        record_seq_watermark(&active, 30);
        assert_eq!(seed_archive_seq(&active), 31);
    }

    #[test]
    fn archive_sequence_resumes_above_existing_archives() {
        // A restart recovers the counter from the names on disk. Reusing a
        // number would leave two archives ordered by an ambiguous key.
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("runtime-trace.jsonl");
        fs::write(&active, "").unwrap();
        fs::write(
            tmp.path()
                .join("runtime-trace.0000000007-20260624-031500.jsonl"),
            "x\n",
        )
        .unwrap();
        // A legacy archive contributes no number and must not disturb seeding.
        fs::write(
            tmp.path().join("runtime-trace.20260101-000000.jsonl"),
            "y\n",
        )
        .unwrap();

        assert_eq!(
            seed_archive_seq(&active),
            8,
            "the series must resume above the highest number already on disk"
        );
    }

    #[test]
    fn split_base_ext_cases() {
        assert_eq!(
            split_base_ext("runtime-trace.jsonl"),
            ("runtime-trace", ".jsonl")
        );
        assert_eq!(split_base_ext("a.b.jsonl"), ("a.b", ".jsonl"));
        assert_eq!(split_base_ext("trace"), ("trace", ""));
        assert_eq!(split_base_ext(".hidden"), (".hidden", ""));
    }

    #[test]
    fn is_archive_core_matches_only_generated_shapes() {
        // `core` is the suffix with the base prefix and extension stripped, i.e.
        // exactly what `archive_path` puts between them: `<stamp>` or
        // `<stamp>.<counter>`.
        assert!(is_archive_core("20260624-031500")); // <stamp>
        assert!(is_archive_core("20260624-031500.1")); // <stamp>.<counter>
        assert!(is_archive_core("20260624-031500.42"));
        assert!(!is_archive_core("20260624-031500.backup"));
        assert!(!is_archive_core("20260624-031500.notes"));
        assert!(!is_archive_core("20260624-031500.1.2")); // counter is not multi-segment
        assert!(!is_archive_core("20260624-031500.")); // empty counter
        // Not a stamp at all.
        assert!(!is_archive_core("notes"));
        assert!(!is_archive_core("migrate.123.456"));
        assert!(!is_archive_core("2026-06-24")); // dashes in the wrong place
        assert!(!is_archive_core("20260624-0315000")); // too long
        // is_stamp is strict about the exact 15-char shape.
        assert!(is_stamp("20260624-031500"));
        assert!(!is_stamp("20260624-03150")); // too short
        assert!(!is_stamp("2026062a-031500")); // non-digit
    }

    #[test]
    fn rotation_through_append_prunes_to_retention_cap() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Every append rotates (max_bytes = 1); retention keeps the newest 2.
        install_rotating(tmp.path(), 1, false, 2, 0);

        for i in 0..10 {
            emit(&format!("event-{i}"));
        }

        flush_for_test().unwrap();
        let path = runtime_trace_path().unwrap();
        // Retention ran as a side effect of the real append path, capping the
        // archive set even though many more rotations occurred.
        assert_eq!(
            list_archives(&path).unwrap().len(),
            2,
            "retention cap should hold across rotations driven by append_line"
        );
    }

    #[test]
    fn rotating_size_and_daily_both_active() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Both triggers on, no retention so every event is preserved.
        install_rotating(tmp.path(), 200, true, 0, 0);

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed a stale (two-days-ago) active file.
        fs::write(&path, "{\"message\":\"old-day\"}\n").unwrap();
        set_mtime(&path, SystemTime::now() - Duration::from_secs(2 * 86_400));

        for i in 0..20 {
            emit(&format!("burst-{i}"));
        }

        flush_for_test().unwrap();
        let archives = list_archives(&path).unwrap();
        // Daily rotation archives the stale file; size rotation adds more.
        assert!(
            archives.len() >= 2,
            "expected a daily archive plus size archives, got {}",
            archives.len()
        );
        // 1 seeded event + 20 emitted, all preserved across active + archives.
        assert_eq!(total_events(&path), 21, "no events lost with both triggers");
        // The stale day's event lives in an archive, never in the active file.
        // (The active file may not exist if the final append also rotated; it is
        // recreated lazily on the next append, exactly as the reader expects.)
        let active = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !active.contains("old-day"),
            "stale day's event must not remain in the active file"
        );
        let archived_old_day = list_archives(&path).unwrap().iter().any(|(p, _)| {
            fs::read_to_string(p)
                .unwrap_or_default()
                .contains("old-day")
        });
        assert!(
            archived_old_day,
            "stale day's event must be preserved in an archive"
        );
    }

    #[test]
    fn rotating_extensionless_path_isolates_foreign_siblings() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Custom path with no extension; every append rotates; keep newest 1.
        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_path: tmp.path().join("trace").to_string_lossy().into_owned(),
            log_persistence_max_bytes: 1,
            log_persistence_rotate_daily: false,
            log_persistence_retention_max_files: 1,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());
        let path = runtime_trace_path().unwrap();

        let foreign_plain = tmp.path().join("trace.notes");
        let foreign_stamped = tmp.path().join("trace.20260101-000000.notes");
        fs::write(&foreign_plain, "keep me\n").unwrap();
        fs::write(&foreign_stamped, "keep me too\n").unwrap();

        for i in 0..6 {
            emit(&format!("e-{i}"));
        }

        flush_for_test().unwrap();
        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "retention cap applies for extension-less paths"
        );
        assert!(
            archives
                .iter()
                .all(|(p, _)| p != &foreign_plain && p != &foreign_stamped),
            "foreign siblings must not be classified as archives"
        );
        assert!(
            foreign_plain.exists() && foreign_stamped.exists(),
            "foreign siblings must survive retention"
        );
        for (p, _) in &archives {
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(
                is_archive_core(name.strip_prefix("trace.").unwrap()),
                "archive {name} must carry the exact archive shape"
            );
        }
    }

    #[test]
    fn retention_spares_stamp_prefixed_foreign_sibling_with_extension() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Default `.jsonl` path; every append rotates; keep newest 1.
        install_rotating(tmp.path(), 1, false, 1, 0);

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();

        // A foreign file that starts with a valid stamp and carries the `.jsonl`
        // extension, but is NOT a shape `archive_path` generates. It must never
        // be classified as an archive nor pruned, no matter how many real
        // rotations the retention cap triggers.
        let foreign = dir.join("runtime-trace.20260101-000000.backup.jsonl");
        fs::write(&foreign, "do not delete\n").unwrap();

        for i in 0..6 {
            emit(&format!("e-{i}"));
        }

        flush_for_test().unwrap();
        let archives = list_archives(&path).unwrap();
        assert!(
            archives.iter().all(|(p, _)| p != &foreign),
            "stamp-prefixed foreign sibling must not be classified as an archive"
        );
        assert!(
            foreign.exists(),
            "stamp-prefixed foreign sibling must survive retention"
        );
        // Real archives are still pruned to the cap.
        assert_eq!(archives.len(), 1, "real archives are still capped");
    }

    #[test]
    fn rotating_entry_count_rotation_trigger() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_max_bytes: 0,
            log_persistence_rotate_daily: false,
            log_persistence_retention_max_files: 0,
            log_persistence_max_entries_per_segment: 2,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());
        let path = runtime_trace_path().unwrap();

        emit("event-1");
        emit("event-2");
        // The cap is 2; the active file now holds exactly cap entries, so the
        // next write triggers rotation (count >= cap after the append).
        let archives_before = list_archives(&path).unwrap();
        assert_eq!(
            archives_before.len(),
            1,
            "rotation must fire when segment reaches cap (>= cap trigger)"
        );
        emit("event-3");
        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "entry-count cap must rotate the active file after cap+1 writes"
        );
        assert_eq!(
            total_events(&path),
            3,
            "all events preserved across active + archive"
        );
    }

    #[test]
    fn entry_count_seeded_at_cap_rotates_before_the_next_append() {
        // An operator enabling entry-count rotation on an existing log (or
        // reloading from another policy) can start with an active file that
        // already holds exactly `cap` rows. The counter is seeded from that
        // file, so the next append must rotate *first*: appending before
        // rotating would archive `cap + 1` rows and break the documented
        // "maximum N entries per segment" contract.
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        const CAP: usize = 3;

        // Seed an active file with exactly CAP non-empty JSONL rows before
        // any writer is installed.
        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let seeded = state_dir.join("runtime-trace.jsonl");
        let mut seed = String::new();
        for i in 0..CAP {
            let ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
            let mut v = serde_json::to_value(&ev).unwrap();
            v["message"] = serde_json::Value::String(format!("seeded-{i}"));
            seed.push_str(&serde_json::to_string(&v).unwrap());
            seed.push('\n');
        }
        fs::write(&seeded, &seed).unwrap();
        assert_eq!(count_nonempty_lines(&seeded).unwrap(), CAP);

        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_max_bytes: 0,
            log_persistence_rotate_daily: false,
            log_persistence_retention_max_files: 0,
            log_persistence_max_entries_per_segment: CAP,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());
        let path = runtime_trace_path().unwrap();
        assert_eq!(path, seeded, "writer must adopt the seeded active file");

        emit("post-seed");

        // No archive may exceed the cap.
        for (archive, _) in list_archives(&path).unwrap() {
            let n = count_nonempty_lines(&archive).unwrap();
            assert!(
                n <= CAP,
                "archive {} holds {n} entries, exceeding the cap of {CAP}",
                archive.display()
            );
        }
        // Nothing is lost: the seeded rows plus the new one are all on disk.
        assert_eq!(
            total_events(&path),
            CAP + 1,
            "seeded rows and the new append must all survive the rotation"
        );
    }

    #[test]
    fn entry_count_seeded_over_cap_archives_the_existing_file_whole() {
        // Transition case: enabling entry-count rotation on a log that is
        // already larger than the cap, or reloading after the cap was lowered.
        // Splitting that file into cap-sized pieces would mean a full rewrite,
        // which is the O(file size) cost this trigger exists to avoid, so the
        // existing file is archived whole, once. Every archive after it holds
        // exactly `cap` entries.
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        const CAP: usize = 2;
        const SEEDED: usize = 5; // deliberately over the cap

        let state_dir = tmp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let seeded = state_dir.join("runtime-trace.jsonl");
        let mut seed = String::new();
        for i in 0..SEEDED {
            let ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
            let mut v = serde_json::to_value(&ev).unwrap();
            v["message"] = serde_json::Value::String(format!("seeded-{i}"));
            seed.push_str(&serde_json::to_string(&v).unwrap());
            seed.push('\n');
        }
        fs::write(&seeded, &seed).unwrap();

        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_max_bytes: 0,
            log_persistence_rotate_daily: false,
            log_persistence_retention_max_files: 0,
            log_persistence_max_entries_per_segment: CAP,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());
        let path = runtime_trace_path().unwrap();

        emit("post-seed");

        // The transition archive holds the whole over-cap file, not `cap` rows.
        let archives = list_archives(&path).unwrap();
        assert_eq!(archives.len(), 1, "the seeded file is archived once");
        assert_eq!(
            count_nonempty_lines(&archives[0].0).unwrap(),
            SEEDED,
            "the pre-existing file is archived whole rather than split"
        );

        // Steady state resumes: the next `cap` appends produce a cap-sized archive.
        for i in 0..CAP {
            emit(&format!("steady-{i}"));
        }
        let archives = list_archives(&path).unwrap();
        assert_eq!(archives.len(), 2, "a second rotation fired at the cap");
        let mut sizes: Vec<usize> = archives
            .iter()
            .map(|(p, _)| count_nonempty_lines(p).unwrap())
            .collect();
        sizes.sort_unstable();
        assert_eq!(
            sizes,
            vec![CAP, SEEDED],
            "after the one-time transition archive, archives are cap-sized"
        );

        // Nothing is lost across the transition.
        assert_eq!(total_events(&path), SEEDED + 1 + CAP);
    }
}
