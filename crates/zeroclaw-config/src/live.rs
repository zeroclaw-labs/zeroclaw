//! Live config publication storage.
//!
//! One canonical published pair — a [`Config`] plus its opaque
//! [`ConfigRevision`] — behind a single short reader lock. The
//! inward-facing types here are storage and access only: who may publish,
//! when writes serialize, and how commits are retained are owned by the
//! runtime's `LiveConfigAuthority` transaction, not by this module.
//!
//! Design constraints (issue 10892 / RFC 7897, ADR-012):
//!
//! * Readers observe the config and its revision as one pair; there is no
//!   way to read a config from one publication and a revision from
//!   another.
//! * [`LiveConfigHandle`] is read-only. It exposes no write guard and no
//!   writable `Arc`; publication happens only through the [`LiveConfig`]
//!   storage owner held by the authority.
//! * The revision is an opaque authority epoch (a random UUID) plus a
//!   checked sequence. Sequences compare only within one epoch; epochs
//!   are never ordered. A cloned [`LiveConfig`] retains its epoch, so
//!   authority clones and gateway retries share publication identity,
//!   while a full replacement constructs a fresh epoch.
//! * Advancing the sequence is checked: an exhausted epoch refuses to
//!   allocate the next revision *before* any irreversible persistence
//!   starts, and publication installs only the exact successor revision
//!   that was allocated.

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::schema::Config;

/// Opaque identity of one authority generation's publication domain.
///
/// Random (UUID v4) and deliberately unordered: two epochs are either
/// equal or distinct, never "older"/"newer". Recency is judged by
/// comparing sequences within one epoch only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConfigEpoch(Uuid);

impl ConfigEpoch {
    /// Allocate a fresh, previously unused epoch.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ConfigEpoch {
    fn default() -> Self {
        Self::new()
    }
}

/// Identity of one published config: authority epoch plus sequence.
///
/// Copyable and comparable for equality only. Order-sensitive questions
/// must go through [`ConfigRevision::same_epoch`] and
/// [`ConfigRevision::succeeds_within_epoch`], which refuse to compare
/// sequences across different epochs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConfigRevision {
    epoch: ConfigEpoch,
    sequence: u64,
}

impl ConfigRevision {
    /// The first revision of a fresh epoch.
    pub(crate) fn initial(epoch: ConfigEpoch) -> Self {
        Self { epoch, sequence: 0 }
    }

    /// Whether both revisions belong to the same authority epoch.
    pub fn same_epoch(&self, other: &Self) -> bool {
        self.epoch == other.epoch
    }

    /// Whether `self` is a later publication than `earlier`, comparing
    /// sequences only within a shared epoch. Two revisions from different
    /// epochs are never ordered; this returns `false` for them.
    pub fn succeeds_within_epoch(&self, earlier: &Self) -> bool {
        self.epoch == earlier.epoch && self.sequence > earlier.sequence
    }
}

/// Failures of the publication storage itself.
#[derive(Debug, thiserror::Error)]
pub enum LiveConfigError {
    /// The epoch's sequence space is exhausted, so no further publication
    /// identity is representable. Surfaced before irreversible
    /// persistence, never after a save.
    #[error("config revision sequence is exhausted for this authority epoch")]
    RevisionExhausted,
    /// A publication attempted to install a revision that is not the
    /// exact successor of the currently published one (for example a
    /// stale or repeated allocation). The published pair is unchanged.
    #[error("refusing to publish revision {allocated:?} over the current {current:?}")]
    NotSuccessor {
        allocated: ConfigRevision,
        current: ConfigRevision,
    },
}

/// The private published pair. Always read and written as one unit under
/// the storage lock; no code path can observe a config from one
/// publication paired with another revision.
struct PublishedPair {
    config: Config,
    revision: ConfigRevision,
}

/// Canonical live-config storage for one authority generation.
///
/// Cloning shares the storage (and therefore the epoch and sequence);
/// constructing a new `LiveConfig` starts a fresh epoch whose initial
/// publication is sequence zero. The paired state is private: readers go
/// through [`LiveConfigHandle`], and publication goes through
/// [`LiveConfig::next_revision`] plus [`LiveConfig::publish`] under the
/// owning transaction's serialization.
#[derive(Clone)]
pub struct LiveConfig {
    pair: Arc<RwLock<PublishedPair>>,
}

impl LiveConfig {
    /// Create the storage with `config` as the initial publication of a
    /// fresh epoch.
    pub fn new(config: Config) -> Self {
        Self {
            pair: Arc::new(RwLock::new(PublishedPair {
                config,
                revision: ConfigRevision::initial(ConfigEpoch::new()),
            })),
        }
    }

    /// The read-only facade over this storage.
    pub fn handle(&self) -> LiveConfigHandle {
        LiveConfigHandle {
            pair: Arc::clone(&self.pair),
        }
    }

    /// The epoch all publications of this storage belong to.
    pub fn epoch(&self) -> ConfigEpoch {
        self.pair.read().revision.epoch
    }

    /// The currently published revision.
    pub fn published_revision(&self) -> ConfigRevision {
        self.pair.read().revision
    }

    /// Clone the currently published config.
    pub fn snapshot(&self) -> Config {
        self.pair.read().config.clone()
    }

    /// Allocate the next publication identity, checking
    /// representability. Call before any irreversible persistence: an
    /// exhausted epoch must fail before disk state can change, not after.
    pub fn next_revision(&self) -> Result<ConfigRevision, LiveConfigError> {
        let current = self.pair.read().revision;
        // Checked advancement, never wrapping: a wrapped sequence would
        // silently alias an earlier publication of the same epoch.
        let sequence = current
            .sequence
            .checked_add(1)
            .ok_or(LiveConfigError::RevisionExhausted)?;
        Ok(ConfigRevision {
            epoch: current.epoch,
            sequence,
        })
    }

    /// Install `config` as the published pair under exactly `revision`.
    ///
    /// `revision` must be the successor of the currently published
    /// revision within the same epoch; anything else (stale, repeated, or
    /// cross-epoch) is refused with the published pair unchanged. The
    /// write lock is held only for the pair install — never across
    /// persistence I/O, which the caller performs between allocation and
    /// this call.
    pub fn publish(
        &self,
        revision: ConfigRevision,
        config: Config,
    ) -> Result<ConfigRevision, LiveConfigError> {
        let mut pair = self.pair.write();
        // Checked successor comparison: past `u64::MAX` no successor exists,
        // so the storage contract itself rejects a wrapping publication
        // identity (and `next_revision` already refuses to allocate one).
        let is_successor = pair.revision.sequence.checked_add(1) == Some(revision.sequence);
        if revision.epoch != pair.revision.epoch || !is_successor {
            return Err(LiveConfigError::NotSuccessor {
                allocated: revision,
                current: pair.revision,
            });
        }
        pair.config = config;
        pair.revision = revision;
        Ok(revision)
    }
}

/// Read-only live config facade.
///
/// Clones share the same canonical storage. The handle dereferences to
/// the published [`Config`] for the duration of one short read guard and
/// can also report the paired [`ConfigRevision`] under that same guard;
/// it exposes no write path and never hands out a writable `Arc`.
#[derive(Clone)]
pub struct LiveConfigHandle {
    pair: Arc<RwLock<PublishedPair>>,
}

impl LiveConfigHandle {
    /// Whether two handles observe the same canonical storage (same
    /// authority generation). Diagnostics and identity tests only; it
    /// says nothing about publication recency.
    pub fn same_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pair, &other.pair)
    }

    /// One short paired read of the published config.
    pub fn read(&self) -> LiveConfigReadGuard<'_> {
        LiveConfigReadGuard {
            guard: self.pair.read(),
        }
    }

    /// Clone the currently published config.
    pub fn snapshot(&self) -> Config {
        self.pair.read().config.clone()
    }

    /// Clone the published config together with its revision, observed
    /// as one pair.
    pub fn snapshot_with_revision(&self) -> (Config, ConfigRevision) {
        let pair = self.pair.read();
        (pair.config.clone(), pair.revision)
    }

    /// The currently published revision.
    pub fn revision(&self) -> ConfigRevision {
        self.pair.read().revision
    }
}

/// Short-lived paired read of the published config and its revision.
///
/// Dereferences to [`Config`] so ordinary read sites keep their shape.
/// The guard is a `parking_lot` read guard: it is `!Send` and must never
/// be held across an `.await` — clone out what crosses an async
/// boundary.
pub struct LiveConfigReadGuard<'a> {
    guard: parking_lot::RwLockReadGuard<'a, PublishedPair>,
}

impl std::ops::Deref for LiveConfigReadGuard<'_> {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.guard.config
    }
}

impl AsRef<Config> for LiveConfigReadGuard<'_> {
    fn as_ref(&self) -> &Config {
        &self.guard.config
    }
}

impl LiveConfigReadGuard<'_> {
    /// The revision of the config this guard exposes, observed under the
    /// same lock acquisition as the config itself.
    pub fn revision(&self) -> ConfigRevision {
        self.guard.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_agent(alias: &str) -> Config {
        let mut config = Config::default();
        config.agents.insert(alias.to_string(), Default::default());
        config
    }

    #[test]
    fn fresh_storage_gets_a_fresh_epoch_and_zero_sequence() {
        let a = LiveConfig::new(Config::default());
        let b = LiveConfig::new(Config::default());
        let revision_a = a.published_revision();
        let revision_b = b.published_revision();

        assert!(!revision_a.same_epoch(&revision_b));
        assert_eq!(a.epoch(), revision_a.epoch);
    }

    #[test]
    fn clones_share_storage_epoch_and_publications() {
        let live = LiveConfig::new(config_with_agent("alpha"));
        let cloned = live.clone();
        let handle = live.handle();

        assert!(handle.same_storage(&cloned.handle()));
        assert_eq!(live.epoch(), cloned.epoch());
        let next = live.next_revision().unwrap();
        live.publish(next, config_with_agent("beta")).unwrap();
        assert_eq!(cloned.published_revision(), next);
        assert_eq!(handle.snapshot().agents.len(), 1);
        assert!(handle.snapshot().agents.contains_key("beta"));
    }

    #[test]
    fn publication_requires_the_exact_allocated_successor() {
        let live = LiveConfig::new(Config::default());
        // `next_revision` observes the published pair without mutating it,
        // so every allocation before a publication describes the same
        // successor.
        let first = live.next_revision().unwrap();
        let first_again = live.next_revision().unwrap();
        assert_eq!(first, first_again);

        // The allocated successor publishes exactly once; repeating it is
        // refused as a stale publication.
        live.publish(first, config_with_agent("alpha")).unwrap();
        assert!(matches!(
            live.publish(first_again, Config::default()),
            Err(LiveConfigError::NotSuccessor { .. })
        ));

        // After that publication, the next allocation is the new successor;
        // the previous one is now stale.
        let second = live.next_revision().unwrap();
        assert!(second.succeeds_within_epoch(&first));
        assert!(matches!(
            live.publish(first, Config::default()),
            Err(LiveConfigError::NotSuccessor { .. })
        ));
        live.publish(second, config_with_agent("beta")).unwrap();
        assert!(live.snapshot().agents.contains_key("beta"));
    }

    #[test]
    fn publication_from_another_epoch_is_refused() {
        let live = LiveConfig::new(Config::default());
        let foreign = LiveConfig::new(Config::default());
        let foreign_next = foreign.next_revision().unwrap();

        assert!(matches!(
            live.publish(foreign_next, Config::default()),
            Err(LiveConfigError::NotSuccessor { .. })
        ));
    }

    #[test]
    fn exhausted_epoch_refuses_allocation_before_persistence() {
        let live = LiveConfig::new(Config::default());
        {
            let mut pair = live.pair.write();
            pair.revision.sequence = u64::MAX;
        }

        assert!(matches!(
            live.next_revision(),
            Err(LiveConfigError::RevisionExhausted)
        ));
        // With no representable successor, the storage contract also
        // refuses any publication attempt outright — wraparound cannot
        // alias an earlier identity of this epoch.
        let phantom = ConfigRevision {
            epoch: live.epoch(),
            sequence: 0,
        };
        assert!(matches!(
            live.publish(phantom, Config::default()),
            Err(LiveConfigError::NotSuccessor { .. })
        ));
        // The exhausted pair stays published and readable.
        assert_eq!(live.published_revision().sequence, u64::MAX);
    }

    #[test]
    fn cross_epoch_sequences_are_never_ordered() {
        let a = LiveConfig::new(Config::default());
        let b = LiveConfig::new(Config::default());
        // Two fresh storages publish their initial revisions: equal
        // sequence numbers by construction, different epochs.
        let a_initial = a.published_revision();
        let b_initial = b.published_revision();
        let a_next = a.next_revision().unwrap();
        a.publish(a_next, Config::default()).unwrap();

        assert!(!a_initial.same_epoch(&b_initial));
        assert!(!a_initial.succeeds_within_epoch(&b_initial));
        assert!(!b_initial.succeeds_within_epoch(&a_initial));
        // Advancing within one epoch orders only that epoch's revisions.
        assert!(a_next.succeeds_within_epoch(&a_initial));
        assert!(!a_next.succeeds_within_epoch(&b_initial));
    }

    #[test]
    fn read_guard_pairs_config_with_its_revision() {
        let live = LiveConfig::new(config_with_agent("alpha"));
        let handle = live.handle();
        let next = live.next_revision().unwrap();
        live.publish(next, config_with_agent("beta")).unwrap();

        let guard = handle.read();
        assert!(guard.agents.contains_key("beta"));
        assert_eq!(guard.revision(), next);
        // The revision observed under the guard matches the snapshot pair.
        let (config, revision) = handle.snapshot_with_revision();
        assert_eq!(revision, guard.revision());
        assert!(config.agents.contains_key("beta"));
    }
}
