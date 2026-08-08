//! Process-shared Channel conversation identity, history, and turn lifecycle.
//!
//! The four Channel webhooks (WhatsApp, Linq, WATI, Nextcloud Talk) and the
//! Channel orchestrator resolve one opaque cross-turn conversation id per
//! routing/storage `conversation_history_key` through this shared state, so a
//! re-delivered or follow-up message reuses the same id instead of minting a
//! fresh UUID per inbound request. In durable mode the backend session record
//! is the single source of truth and the id is never mirrored into the cache;
//! in memory-only mode the bounded LRU owns both the id and the history as one
//! record.
//!
//! The captured conversation id is also a write fence: history append / update
//! / rollback / compaction are conditional on the record still carrying the id
//! the turn captured before any async work began. A stale or deleted result is
//! an expected lifecycle race, not retried, and must not recreate a record.
//! Active Channel turn workers register cancellation tokens here so `/new`,
//! `/clear`, and delete can stop competing workers before deleting the record;
//! the conditional write remains the final correctness boundary.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::model_provider::ChatMessage;

use crate::session_backend::{ConditionalSessionWrite, SessionBackend};

/// Bound on the memory-only session LRU. Matches the orchestrator's
/// per-sender history bound so the record (history + id together) churns at
/// the same rate as before.
pub const MAX_CHANNEL_SESSIONS: usize = 1000;

/// In-memory history + conversation id for a memory-only session. The two are
/// ONE LRU record so they evict together: a history eviction never strands a
/// stale id, and an id rotation never orphans old history.
#[derive(Debug, Clone)]
struct MemorySessionRecord {
    history: Vec<ChatMessage>,
    conversation_id: String,
}

/// Cached view of one session. `Memory` owns both history and id (memory-only
/// mode). `DurableHistory` is a bounded materialized view of backend history
/// only - the id is never mirrored here, it is resolved from the backend each
/// time. The enum keeps the two modes from sharing storage.
enum CachedChannelSession {
    Memory(MemorySessionRecord),
    DurableHistory(Vec<ChatMessage>),
}

/// Shared Channel conversation identity, history, and turn lifecycle for one
/// daemon iteration.
///
/// One instance is constructed per reload iteration in the daemon and cloned
/// (`Arc`) into both the gateway and the channel orchestrator, so an inbound
/// webhook and the orchestrator's own turn mint site agree on the same id for
/// a given `conversation_history_key`.
pub struct ChannelConversationStore {
    backend: Option<Arc<dyn SessionBackend>>,
    cache: Mutex<lru::LruCache<String, CachedChannelSession>>,
    persistence_locks: TokioMutex<HashMap<String, Arc<TokioMutex<()>>>>,
    in_flight: TokioMutex<HashMap<String, Vec<CancellationToken>>>,
}

impl ChannelConversationStore {
    async fn persistence_lock(&self, history_key: &str) -> Arc<TokioMutex<()>> {
        let mut locks = self.persistence_locks.lock().await;
        Arc::clone(
            locks
                .entry(history_key.to_string())
                .or_insert_with(|| Arc::new(TokioMutex::new(()))),
        )
    }

    /// Open the complete current record, creating it when absent.
    pub async fn open(
        &self,
        history_key: &str,
    ) -> std::io::Result<crate::ChannelConversationRecord> {
        let lock = self.persistence_lock(history_key).await;
        let _guard = lock.lock().await;
        if let Some(backend) = &self.backend {
            return backend.open_conversation(history_key);
        }
        let mut cache = self.cache.lock();
        if let Some(CachedChannelSession::Memory(record)) = cache.get(history_key) {
            return Ok(crate::ChannelConversationRecord {
                conversation_id: record.conversation_id.clone(),
                history: record.history.clone(),
            });
        }
        let record = MemorySessionRecord {
            history: Vec::new(),
            conversation_id: uuid::Uuid::new_v4().to_string(),
        };
        let opened = crate::ChannelConversationRecord {
            conversation_id: record.conversation_id.clone(),
            history: Vec::new(),
        };
        cache.put(
            history_key.to_string(),
            CachedChannelSession::Memory(record),
        );
        Ok(opened)
    }

    /// Check whether an existing record still carries `conversation_id`.
    pub async fn is_current(
        &self,
        history_key: &str,
        conversation_id: &str,
    ) -> std::io::Result<bool> {
        let lock = self.persistence_lock(history_key).await;
        let _guard = lock.lock().await;
        if let Some(backend) = &self.backend {
            return Ok(backend
                .current_conversation_id(history_key)?
                .is_some_and(|current| current == conversation_id));
        }
        let cache = self.cache.lock();
        Ok(
            matches!(cache.peek(history_key), Some(CachedChannelSession::Memory(rec)) if rec.conversation_id == conversation_id),
        )
    }

    /// Apply a record-scoped mutation and synchronize the cached history.
    pub async fn mutate_if_current(
        &self,
        history_key: &str,
        conversation_id: &str,
        mutation: crate::SessionMutation<'_>,
    ) -> std::io::Result<crate::ConditionalSessionWrite> {
        let lock = self.persistence_lock(history_key).await;
        let _guard = lock.lock().await;
        if let Some(backend) = &self.backend {
            let status =
                backend.mutate_conversation_if_current(history_key, conversation_id, mutation)?;
            if status == crate::ConditionalSessionWrite::Applied {
                self.cache.lock().pop(history_key);
            }
            return Ok(status);
        }
        let mut cache = self.cache.lock();
        match cache.get_mut(history_key) {
            Some(CachedChannelSession::Memory(record))
                if record.conversation_id == conversation_id =>
            {
                match mutation {
                    crate::SessionMutation::Append(message) => record.history.push(message.clone()),
                    crate::SessionMutation::RemoveLast {
                        expected_role,
                        expected_content,
                    } => {
                        if record.history.last().is_some_and(|message| {
                            message.role == expected_role && message.content == expected_content
                        }) {
                            record.history.pop();
                        }
                    }
                    crate::SessionMutation::UpdateLast(message) => {
                        if let Some(last) = record.history.last_mut() {
                            *last = message.clone();
                        }
                    }
                }
                Ok(crate::ConditionalSessionWrite::Applied)
            }
            Some(CachedChannelSession::Memory(_)) => Ok(crate::ConditionalSessionWrite::Stale),
            _ => Ok(crate::ConditionalSessionWrite::Deleted),
        }
    }

    /// Delete the complete conversation record.
    pub async fn delete(&self, history_key: &str) -> std::io::Result<bool> {
        self.cancel_in_flight(history_key).await;
        let lock = self.persistence_lock(history_key).await;
        let _guard = lock.lock().await;
        let existed = if let Some(backend) = &self.backend {
            backend.delete_session(history_key)?
        } else {
            self.cache.lock().pop(history_key).is_some()
        };
        if self.backend.is_some() {
            self.cache.lock().pop(history_key);
        }
        Ok(existed)
    }

    pub async fn register_in_flight(&self, history_key: &str, token: CancellationToken) {
        self.in_flight
            .lock()
            .await
            .entry(history_key.to_string())
            .or_default()
            .push(token);
    }

    pub async fn unregister_in_flight(&self, history_key: &str, token: &CancellationToken) {
        token.cancel();
        let mut in_flight = self.in_flight.lock().await;
        if let Some(tokens) = in_flight.get_mut(history_key) {
            tokens.retain(|registered| !registered.is_cancelled());
            if tokens.is_empty() {
                in_flight.remove(history_key);
            }
        }
    }

    pub async fn cancel_in_flight(&self, history_key: &str) {
        let tokens = self
            .in_flight
            .lock()
            .await
            .get(history_key)
            .cloned()
            .unwrap_or_default();
        for token in tokens {
            token.cancel();
        }
    }

    pub async fn delete_session(&self, history_key: &str) -> std::io::Result<bool> {
        self.delete(history_key).await
    }

    /// Wrap an optional durable backend. `None` selects memory-only mode.
    pub fn new(backend: Option<Arc<dyn SessionBackend>>) -> Self {
        Self {
            backend,
            cache: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(MAX_CHANNEL_SESSIONS)
                    .expect("channel session capacity is non-zero"),
            )),
            persistence_locks: TokioMutex::new(HashMap::new()),
            in_flight: TokioMutex::new(HashMap::new()),
        }
    }

    /// Test-only constructor with an explicit LRU capacity so the eviction
    /// invariant (history + id evict together) can be exercised without
    /// inserting 1000 records.
    #[cfg(test)]
    fn with_capacity(backend: Option<Arc<dyn SessionBackend>>, capacity: usize) -> Self {
        Self {
            backend,
            cache: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(capacity).expect("test capacity is non-zero"),
            )),
            persistence_locks: TokioMutex::new(HashMap::new()),
            in_flight: TokioMutex::new(HashMap::new()),
        }
    }

    /// The durable backend, if any. The orchestrator reuses this handle for
    /// history persistence instead of opening a second backend owner.
    pub fn backend(&self) -> Option<&Arc<dyn SessionBackend>> {
        self.backend.as_ref()
    }

    /// Whether the memory-only store currently owns `history_key`.
    pub fn contains_memory_record(&self, history_key: &str) -> bool {
        self.backend.is_none()
            && matches!(
                self.cache.lock().peek(history_key),
                Some(CachedChannelSession::Memory(_))
            )
    }

    // ── conversation-id resolve / rotate ───────────────────────────────

    /// Resolve the opaque cross-turn conversation id for a `history_key`.
    ///
    /// In durable mode the backend record is the single source of truth: the
    /// id is resolve-or-created there and NEVER mirrored into the cache. In
    /// memory-only mode the LRU owns a `Memory` record (history + id); a fresh
    /// UUID is minted on first resolve and reused until rotation. On backend
    /// failure the error is propagated verbatim; no fallback id is minted.
    pub fn resolve_conversation_id(&self, history_key: &str) -> std::io::Result<String> {
        if let Some(backend) = &self.backend {
            return backend.resolve_or_create_conversation_id(history_key);
        }

        let mut cache = self.cache.lock();
        if let Some(CachedChannelSession::Memory(rec)) = cache.get(history_key) {
            return Ok(rec.conversation_id.clone());
        }
        let id = uuid::Uuid::new_v4().to_string();
        cache.put(
            history_key.to_string(),
            CachedChannelSession::Memory(MemorySessionRecord {
                history: Vec::new(),
                conversation_id: id.clone(),
            }),
        );
        Ok(id)
    }

    // ── history read / conditional mutations ───────────────────────────

    /// Load the current history view for a key.
    ///
    /// Durable: return the cached `DurableHistory` view if present, else load
    /// from the backend and cache it (bounded). Backend failures are returned
    /// without installing a cache entry. Memory-only: return the `Memory`
    /// record's history (empty if the record is absent).
    pub fn load_history(&self, key: &str) -> std::io::Result<Vec<ChatMessage>> {
        if let Some(backend) = &self.backend {
            let mut cache = self.cache.lock();
            if let Some(CachedChannelSession::DurableHistory(history)) = cache.get(key) {
                return Ok(history.clone());
            }
            // Keep backend materialization and cache installation atomic with
            // lifecycle invalidation. Reset/delete release their backend lock
            // before taking this cache lock, so this order cannot form a cycle.
            let messages = backend.load_fallible(key)?;
            cache.put(
                key.to_string(),
                CachedChannelSession::DurableHistory(messages.clone()),
            );
            Ok(messages)
        } else {
            let mut cache = self.cache.lock();
            Ok(match cache.get(key) {
                Some(CachedChannelSession::Memory(rec)) => rec.history.clone(),
                _ => Vec::new(),
            })
        }
    }

    /// Prime the durable history cache view for `key` with `messages`.
    ///
    /// Used by startup rehydration to install a repaired / pruned view (orphan
    /// user-turn closure markers added, orphaned tool messages removed) so the
    /// first turn does not re-read the un-pruned backend history. No-op in
    /// memory-only mode (rehydration only runs when a backend is present). The
    /// conversation id is NOT mirrored - this only installs a `DurableHistory`
    /// view; the id is still resolved from the backend on demand.
    pub fn prime_durable_history(&self, key: &str, messages: Vec<ChatMessage>) {
        if self.backend.is_some() {
            let mut cache = self.cache.lock();
            cache.put(
                key.to_string(),
                CachedChannelSession::DurableHistory(messages),
            );
        }
    }

    /// Append `message` iff the record still carries `expected_id`. Durable
    /// delegates to the backend conditional method and only updates the
    /// `DurableHistory` cache view on `Applied`; stale/deleted leaves the cache
    /// untouched. Memory-only compares the id under one cache lock and mutates
    /// in place. `max_history` bounds the retained tail.
    pub fn append_history_if_current(
        &self,
        key: &str,
        expected_id: &str,
        message: ChatMessage,
        max_history: usize,
    ) -> std::io::Result<ConditionalSessionWrite> {
        if let Some(backend) = &self.backend {
            let status = backend.mutate_conversation_if_current(
                key,
                expected_id,
                crate::SessionMutation::Append(&message),
            )?;
            if status == ConditionalSessionWrite::Applied {
                let mut cache = self.cache.lock();
                if let Some(CachedChannelSession::DurableHistory(history)) = cache.get_mut(key) {
                    push_bounded(history, message, max_history);
                }
            }
            return Ok(status);
        }

        let mut cache = self.cache.lock();
        match cache.get_mut(key) {
            Some(CachedChannelSession::Memory(rec)) if rec.conversation_id == expected_id => {
                push_bounded(&mut rec.history, message, max_history);
                Ok(ConditionalSessionWrite::Applied)
            }
            Some(CachedChannelSession::Memory(_)) => Ok(ConditionalSessionWrite::Stale),
            _ => Ok(ConditionalSessionWrite::Deleted),
        }
    }

    /// Remove the last message iff the record still carries `expected_id` and
    /// (memory-only) the last message matches `expected_role`/`expected_content`.
    ///
    /// Durable delegates to the backend conditional `remove_last`; the caller
    /// is responsible for only rolling back a user turn it just appended (and
    /// whose append returned `Applied`), so the orphan is the last row. Turn
    /// serialization (cancel+wait on reset/delete) keeps it the last row; the
    /// id fence is the final boundary for cross-process races. Memory-only does
    /// the content-checked pop atomically under the cache lock. A matching
    /// record whose last message does not match is a no-op `Applied`. The
    /// `Memory` record is kept (NOT evicted) when history empties - a failed
    /// rollback must not rotate the id.
    pub fn rollback_last_if_current(
        &self,
        key: &str,
        expected_id: &str,
        expected_role: &str,
        expected_content: &str,
    ) -> std::io::Result<ConditionalSessionWrite> {
        if let Some(backend) = &self.backend {
            let status = backend.mutate_conversation_if_current(
                key,
                expected_id,
                crate::SessionMutation::RemoveLast {
                    expected_role,
                    expected_content,
                },
            )?;
            if status == ConditionalSessionWrite::Applied {
                let mut cache = self.cache.lock();
                if let Some(CachedChannelSession::DurableHistory(history)) = cache.get_mut(key) {
                    history.pop();
                }
            }
            return Ok(status);
        }

        let mut cache = self.cache.lock();
        match cache.get_mut(key) {
            Some(CachedChannelSession::Memory(rec)) if rec.conversation_id == expected_id => {
                let should_pop = rec
                    .history
                    .last()
                    .is_some_and(|m| m.role == expected_role && m.content == expected_content);
                if should_pop {
                    rec.history.pop();
                }
                Ok(ConditionalSessionWrite::Applied)
            }
            Some(CachedChannelSession::Memory(_)) => Ok(ConditionalSessionWrite::Stale),
            _ => Ok(ConditionalSessionWrite::Deleted),
        }
    }

    /// Rewrite the history view in place iff the record still carries
    /// `expected_id`, via `compact`. Compaction is a cache-only context-budget
    /// optimization - the backend keeps the full history (matching the
    /// pre-fence behavior). Durable gates on the backend id (`session_exists`
    /// first so resolve never recreates a deleted record); memory-only gates on
    /// the in-cache id. Stale/deleted surfaces as such rather than compacting a
    /// doomed view.
    pub fn compact_history_if_current(
        &self,
        key: &str,
        expected_id: &str,
        compact: impl FnOnce(&mut Vec<ChatMessage>),
    ) -> std::io::Result<ConditionalSessionWrite> {
        if let Some(backend) = &self.backend {
            // Serialize the backend view check/load and cache installation with
            // reset/delete cache invalidation. Lifecycle backend operations do
            // not retain their own lock while waiting for this cache lock.
            let mut cache = self.cache.lock();
            let Some(current) = backend.current_conversation_id(key)? else {
                return Ok(ConditionalSessionWrite::Deleted);
            };
            if current != expected_id {
                return Ok(ConditionalSessionWrite::Stale);
            }
            let mut messages = match cache.get(key) {
                Some(CachedChannelSession::DurableHistory(history)) => history.clone(),
                _ => backend.load_fallible(key)?,
            };
            compact(&mut messages);
            cache.put(
                key.to_string(),
                CachedChannelSession::DurableHistory(messages),
            );
            return Ok(ConditionalSessionWrite::Applied);
        }

        let mut cache = self.cache.lock();
        match cache.get_mut(key) {
            Some(CachedChannelSession::Memory(rec)) if rec.conversation_id == expected_id => {
                compact(&mut rec.history);
                Ok(ConditionalSessionWrite::Applied)
            }
            Some(CachedChannelSession::Memory(_)) => Ok(ConditionalSessionWrite::Stale),
            _ => Ok(ConditionalSessionWrite::Deleted),
        }
    }

    /// Number of `Memory` records in the cache. Test-only observable for the
    /// "durable mode never creates a Memory record" invariant; not production
    /// API.
    #[cfg(test)]
    fn memory_variant_count_for_test(&self) -> usize {
        self.cache
            .lock()
            .iter()
            .filter(|(_, v)| matches!(v, CachedChannelSession::Memory(_)))
            .count()
    }

    /// Return the keys currently materialized in the bounded history cache.
    pub fn cached_keys(&self) -> Vec<String> {
        self.cache.lock().iter().map(|(k, _)| k.clone()).collect()
    }

    /// Read the current complete record without creating one on a miss.
    pub async fn existing_record(
        &self,
        history_key: &str,
    ) -> Option<crate::ChannelConversationRecord> {
        if let Some(backend) = &self.backend {
            let id = backend
                .current_conversation_id(history_key)
                .ok()
                .flatten()?;
            return Some(crate::ChannelConversationRecord {
                conversation_id: id,
                history: backend.load(history_key),
            });
        }
        let cache = self.cache.lock();
        match cache.peek(history_key) {
            Some(CachedChannelSession::Memory(rec)) => Some(crate::ChannelConversationRecord {
                conversation_id: rec.conversation_id.clone(),
                history: rec.history.clone(),
            }),
            _ => None,
        }
    }
}

/// Push `message` onto `history` and trim the head beyond `max_history`.
fn push_bounded(history: &mut Vec<ChatMessage>, message: ChatMessage, max_history: usize) {
    history.push(message);
    while history.len() > max_history {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_backend::SessionBackend;
    use crate::session_sqlite::SqliteSessionBackend;
    use std::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};
    use std::sync::mpsc::{Receiver, SyncSender};
    use std::time::Duration;
    use tempfile::TempDir;

    const BLOCK_NONE: u8 = 0;
    const BLOCK_LOAD: u8 = 1;
    const BLOCK_CURRENT_ID: u8 = 2;

    struct BlockingBackend {
        inner: Arc<dyn SessionBackend>,
        block_point: AtomicU8,
        reached: SyncSender<()>,
        release: Mutex<Receiver<()>>,
        lifecycle_done: SyncSender<()>,
    }

    impl BlockingBackend {
        fn maybe_block(&self, point: u8) {
            if self
                .block_point
                .compare_exchange(
                    point,
                    BLOCK_NONE,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                )
                .is_ok()
            {
                self.reached.send(()).unwrap();
                self.release
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .expect("blocked backend operation must be released");
            }
        }
    }

    impl SessionBackend for BlockingBackend {
        fn load(&self, key: &str) -> Vec<ChatMessage> {
            self.inner.load(key)
        }

        fn load_fallible(&self, key: &str) -> std::io::Result<Vec<ChatMessage>> {
            let messages = self.inner.load_fallible(key)?;
            self.maybe_block(BLOCK_LOAD);
            Ok(messages)
        }

        fn append(&self, key: &str, message: &ChatMessage) -> std::io::Result<()> {
            self.inner.append(key, message)
        }

        fn remove_last(&self, key: &str) -> std::io::Result<bool> {
            self.inner.remove_last(key)
        }

        fn list_sessions(&self) -> Vec<String> {
            self.inner.list_sessions()
        }

        fn delete_session(&self, key: &str) -> std::io::Result<bool> {
            let result = self.inner.delete_session(key);
            self.lifecycle_done.send(()).unwrap();
            result
        }

        fn session_exists(&self, key: &str) -> bool {
            self.inner.session_exists(key)
        }

        fn open_conversation(
            &self,
            key: &str,
        ) -> std::io::Result<crate::ChannelConversationRecord> {
            self.inner.open_conversation(key)
        }

        fn current_conversation_id(&self, key: &str) -> std::io::Result<Option<String>> {
            let id = self.inner.current_conversation_id(key)?;
            self.maybe_block(BLOCK_CURRENT_ID);
            Ok(id)
        }

        fn resolve_or_create_conversation_id(&self, key: &str) -> std::io::Result<String> {
            self.inner.resolve_or_create_conversation_id(key)
        }

        fn clear_and_rotate_conversation(&self, key: &str) -> std::io::Result<String> {
            let result = self.inner.clear_and_rotate_conversation(key);
            self.lifecycle_done.send(()).unwrap();
            result
        }

        fn append_if_conversation_matches(
            &self,
            key: &str,
            expected_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<ConditionalSessionWrite> {
            self.inner
                .append_if_conversation_matches(key, expected_id, message)
        }

        fn remove_last_if_conversation_matches(
            &self,
            key: &str,
            expected_id: &str,
        ) -> std::io::Result<ConditionalSessionWrite> {
            self.inner
                .remove_last_if_conversation_matches(key, expected_id)
        }

        fn update_last_if_conversation_matches(
            &self,
            key: &str,
            expected_id: &str,
            message: &ChatMessage,
        ) -> std::io::Result<ConditionalSessionWrite> {
            self.inner
                .update_last_if_conversation_matches(key, expected_id, message)
        }
    }

    struct FailingLoadBackend {
        calls: std::sync::atomic::AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
        history: Vec<ChatMessage>,
    }

    impl SessionBackend for FailingLoadBackend {
        fn load(&self, _: &str) -> Vec<ChatMessage> {
            self.history.clone()
        }

        fn load_fallible(&self, _: &str) -> std::io::Result<Vec<ChatMessage>> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            if self.fail.load(AtomicOrdering::SeqCst) {
                Err(std::io::Error::other("injected history read failure"))
            } else {
                Ok(self.history.clone())
            }
        }

        fn append(&self, _: &str, _: &ChatMessage) -> std::io::Result<()> {
            Ok(())
        }

        fn remove_last(&self, _: &str) -> std::io::Result<bool> {
            Ok(false)
        }

        fn list_sessions(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn failed_history_read_is_not_cached() {
        let backend = Arc::new(FailingLoadBackend {
            calls: std::sync::atomic::AtomicUsize::new(0),
            fail: std::sync::atomic::AtomicBool::new(true),
            history: vec![ChatMessage::user("persisted")],
        });
        let state = ChannelConversationStore::new(Some(backend.clone()));

        assert!(state.load_history("key").is_err());
        assert_eq!(backend.calls.load(AtomicOrdering::SeqCst), 1);

        backend.fail.store(false, AtomicOrdering::SeqCst);
        let history = state.load_history("key").unwrap();
        assert_eq!(history[0].content, "persisted");
        assert_eq!(backend.calls.load(AtomicOrdering::SeqCst), 2);

        let cached = state.load_history("key").unwrap();
        assert_eq!(cached.len(), history.len());
        assert_eq!(cached[0].role, history[0].role);
        assert_eq!(cached[0].content, history[0].content);
        assert_eq!(backend.calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[test]
    fn memory_only_reuses_one_uuid_per_history_key() {
        let state = ChannelConversationStore::new(None);

        let first = state
            .resolve_conversation_id("whatsapp.main_room_alice")
            .unwrap();
        let second = state
            .resolve_conversation_id("whatsapp.main_room_alice")
            .unwrap();
        let other = state
            .resolve_conversation_id("whatsapp.main_room_bob")
            .unwrap();

        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(
            uuid::Uuid::parse_str(&first).unwrap().get_version(),
            Some(uuid::Version::Random)
        );
    }

    #[test]
    fn durable_state_reads_backend_without_mirroring_an_id() {
        let tmp = TempDir::new().unwrap();
        let backend: Arc<dyn SessionBackend> =
            Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let state = ChannelConversationStore::new(Some(Arc::clone(&backend)));

        let first = state
            .resolve_conversation_id("linq.main_chat_alice")
            .unwrap();
        let second = state
            .resolve_conversation_id("linq.main_chat_alice")
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            backend
                .resolve_or_create_conversation_id("linq.main_chat_alice")
                .unwrap(),
            first
        );
        // Resolve does not touch the cache in durable mode.
        assert_eq!(state.memory_variant_count_for_test(), 0);

        // Loading history caches a DurableHistory view, never a Memory record.
        let _ = state.load_history("linq.main_chat_alice");
        assert_eq!(
            state.memory_variant_count_for_test(),
            0,
            "durable mode must never create a Memory record"
        );
    }

    #[test]
    fn memory_only_concurrent_first_resolve_materializes_one_entry() {
        // N threads resolve the same fresh key concurrently: the mutex around
        // the LRU serializes them so exactly ONE UUID wins and is reused, with
        // no duplicate entry materialized under contention.
        let state = Arc::new(ChannelConversationStore::new(None));
        let key = Arc::new("whatsapp.main_room_alice".to_string());
        let n = 8;
        let barrier = Arc::new(std::sync::Barrier::new(n));
        let mut handles = vec![];
        for _ in 0..n {
            let state = Arc::clone(&state);
            let key = Arc::clone(&key);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                state.resolve_conversation_id(&key).unwrap()
            }));
        }
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let first = &ids[0];
        assert!(uuid::Uuid::parse_str(first).is_ok());
        for id in &ids {
            assert_eq!(id, first, "all concurrent resolves must converge on one id");
        }
        assert_eq!(
            state.memory_variant_count_for_test(),
            1,
            "exactly one entry materialized under contention"
        );
    }

    // ── memory LRU + rollback conditional-write tests ─────────────────

    #[test]
    fn memory_lru_evicts_whole_record_history_and_id_together() {
        // capacity-2 cache: insert A/B, touch A, insert C -> B's WHOLE record is
        // evicted. Re-resolving B mints a fresh id + empty history, never a
        // fresh id + old history.
        let state = ChannelConversationStore::with_capacity(None, 2);
        let key_a = "k_a";
        let key_b = "k_b";
        let key_c = "k_c";

        let id_a = state.resolve_conversation_id(key_a).unwrap();
        let id_b = state.resolve_conversation_id(key_b).unwrap();
        state
            .append_history_if_current(key_b, &id_b, ChatMessage::user("b-turn"), 50)
            .unwrap();
        assert_eq!(state.load_history(key_b).unwrap().len(), 1);

        // Touch A so B becomes LRU.
        assert_eq!(state.resolve_conversation_id(key_a).unwrap(), id_a);
        // Insert C -> evicts B entirely.
        let _ = state.resolve_conversation_id(key_c).unwrap();
        assert!(state.load_history(key_b).unwrap().is_empty());

        // Re-resolve B: fresh id, empty history.
        let id_b2 = state.resolve_conversation_id(key_b).unwrap();
        assert_ne!(id_b, id_b2, "evicted key must get a fresh id");
        assert!(
            state.load_history(key_b).unwrap().is_empty(),
            "no old history leaks back"
        );
    }

    #[test]
    fn memory_rollback_keeps_record_and_does_not_rotate_id() {
        let state = ChannelConversationStore::new(None);
        let key = "rollback_key";
        let id = state.resolve_conversation_id(key).unwrap();
        state
            .append_history_if_current(key, &id, ChatMessage::user("failed"), 50)
            .unwrap();
        state
            .rollback_last_if_current(key, &id, "user", "failed")
            .unwrap();
        assert!(state.load_history(key).unwrap().is_empty());
        // The record survives (empty history) and the id is unchanged - a
        // failed rollback must not rotate the id.
        assert_eq!(state.resolve_conversation_id(key).unwrap(), id);
    }

    #[test]
    fn memory_append_after_delete_is_deleted() {
        let state = ChannelConversationStore::new(None);
        let key = "deleted_key";
        let id = state.resolve_conversation_id(key).unwrap();
        // Memory-only delete drops the cache entry.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let existed = runtime.block_on(state.delete_session(key)).unwrap();
        assert!(existed);
        assert_eq!(
            state
                .append_history_if_current(key, &id, ChatMessage::assistant("gone"), 50)
                .unwrap(),
            ConditionalSessionWrite::Deleted
        );
        // A genuinely new resolve mints a fresh id (the deleted worker cannot
        // resurrect the old one).
        let id_after = state.resolve_conversation_id(key).unwrap();
        assert_ne!(id, id_after);
    }

    #[tokio::test]
    async fn delete_cancels_without_waiting_for_worker() {
        let state = ChannelConversationStore::new(None);
        let key = "delete_without_wait";
        state.resolve_conversation_id(key).unwrap();
        let token = CancellationToken::new();
        state.register_in_flight(key, token.clone()).await;

        assert!(state.delete_session(key).await.unwrap());
        assert!(token.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn durable_load_cache_installation_is_atomic_with_reset_invalidation() {
        let tmp = TempDir::new().unwrap();
        let inner: Arc<dyn SessionBackend> =
            Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let key = "durable_load_reset";
        let id_a = inner.resolve_or_create_conversation_id(key).unwrap();
        inner
            .append_if_conversation_matches(key, &id_a, &ChatMessage::user("A"))
            .unwrap();
        let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (lifecycle_tx, lifecycle_rx) = std::sync::mpsc::sync_channel(1);
        let backend_impl = Arc::new(BlockingBackend {
            inner,
            block_point: AtomicU8::new(BLOCK_LOAD),
            reached: reached_tx,
            release: Mutex::new(release_rx),
            lifecycle_done: lifecycle_tx,
        });
        let backend: Arc<dyn SessionBackend> = backend_impl;
        let state = Arc::new(ChannelConversationStore::new(Some(backend)));

        let loader_state = Arc::clone(&state);
        let loader = tokio::task::spawn_blocking(move || loader_state.load_history(key));
        tokio::task::spawn_blocking(move || {
            reached_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("backend operation must reach its blocking point")
        })
        .await
        .unwrap();
        let delete_state = Arc::clone(&state);
        let delete = zeroclaw_spawn::spawn!(async move { delete_state.delete(key).await });
        tokio::task::spawn_blocking(move || {
            lifecycle_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("lifecycle operation must reach the backend")
        })
        .await
        .unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(loader.await.unwrap().unwrap()[0].content, "A");
        assert!(delete.await.unwrap().unwrap());
        assert!(state.load_history(key).unwrap().is_empty());
        let id_b = state.open(key).await.unwrap().conversation_id;
        assert_ne!(id_a, id_b);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn durable_compaction_cache_installation_is_atomic_with_reset_invalidation() {
        let tmp = TempDir::new().unwrap();
        let inner: Arc<dyn SessionBackend> =
            Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let key = "durable_compact_reset";
        let id_a = inner.resolve_or_create_conversation_id(key).unwrap();
        inner
            .append_if_conversation_matches(key, &id_a, &ChatMessage::user("A"))
            .unwrap();
        let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (lifecycle_tx, lifecycle_rx) = std::sync::mpsc::sync_channel(1);
        let backend_impl = Arc::new(BlockingBackend {
            inner,
            block_point: AtomicU8::new(BLOCK_CURRENT_ID),
            reached: reached_tx,
            release: Mutex::new(release_rx),
            lifecycle_done: lifecycle_tx,
        });
        let backend: Arc<dyn SessionBackend> = backend_impl;
        let state = Arc::new(ChannelConversationStore::new(Some(backend)));

        let compact_state = Arc::clone(&state);
        let expected = id_a.clone();
        let compact = tokio::task::spawn_blocking(move || {
            compact_state.compact_history_if_current(key, &expected, |_| {})
        });
        tokio::task::spawn_blocking(move || {
            reached_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("backend operation must reach its blocking point")
        })
        .await
        .unwrap();
        let delete_state = Arc::clone(&state);
        let delete = zeroclaw_spawn::spawn!(async move { delete_state.delete(key).await });
        tokio::task::spawn_blocking(move || {
            lifecycle_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("lifecycle operation must reach the backend")
        })
        .await
        .unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            compact.await.unwrap().unwrap(),
            ConditionalSessionWrite::Applied
        );
        assert!(delete.await.unwrap().unwrap());
        assert!(state.load_history(key).unwrap().is_empty());
        let id_b = state.open(key).await.unwrap().conversation_id;
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn memory_compact_is_conditional_on_id() {
        let state = ChannelConversationStore::new(None);
        let key = "compact_key";
        let id = state.resolve_conversation_id(key).unwrap();
        for content in ["one", "two", "three", "four"] {
            state
                .append_history_if_current(key, &id, ChatMessage::user(content), 50)
                .unwrap();
        }
        assert_eq!(state.load_history(key).unwrap().len(), 4);

        // Compaction with the current id keeps the last 2 messages.
        state
            .compact_history_if_current(key, &id, |history| {
                let keep = history.len().saturating_sub(2);
                history.drain(0..keep);
            })
            .unwrap();
        let compacted = state.load_history(key).unwrap();
        assert_eq!(compacted.len(), 2);
        assert_eq!(compacted[0].content, "three");
        assert_eq!(compacted[1].content, "four");

        // Compaction with a stale id is Stale and does not mutate.
        let stale = uuid::Uuid::new_v4().to_string();
        let status = state
            .compact_history_if_current(key, &stale, |_| {})
            .unwrap();
        assert_eq!(status, ConditionalSessionWrite::Stale);
        assert_eq!(state.load_history(key).unwrap().len(), 2);
    }
}
