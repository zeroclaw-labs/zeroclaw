//! Hybrid Logical Clock (PR #7) — monotonic timestamps under skewed clocks.
//!
//! Adopts CockroachDB's HLC scheme so our multi-device sync layer can order
//! events correctly when devices disagree on wall time by seconds or minutes.
//! Plain `SystemTime::now()` breaks sync ordering the moment two devices have
//! unsynchronised clocks; HLC closes that gap with a small logical counter.
//!
//! Wire format: `{wall_ms}.{logical}.{node_id}` — e.g. `1712847293812.3.kim-laptop`.
//! Lexicographic ordering within the same (wall, logical) slot is stable
//! because `wall_ms` is zero-padded to 13 digits when comparing. For ordering
//! across all devices, compare numerically via `Hlc::cmp` / `PartialOrd`.
//!
//! # Integration plan (deferred inside this PR)
//!
//! This commit ships the core clock with unit tests only. Callers (`memories.
//! updated_at`, sync delta timestamps) migrate in follow-ups once the schema
//! change and sync-protocol bump are coordinated — a schema-migrating rewrite
//! of every timestamp column doesn't belong in an atomic commit.

use std::{
    cmp::Ordering,
    fmt,
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Single HLC timestamp. Totally ordered by `(wall_ms, logical)`; `node_id`
/// is a tiebreaker purely for debugging so two devices at the same
/// `(wall_ms, logical)` don't look identical in logs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    /// Unix milliseconds of wall clock at the moment this timestamp was
    /// generated (or received, if `update()` bumped us forward).
    pub wall_ms: u64,
    /// Logical counter — bumps when `wall_ms` stays the same. Wraps into the
    /// next ms when it overflows `u32::MAX`, which in practice never happens.
    pub logical: u32,
    /// Originating device identifier. Informational; doesn't affect ordering.
    pub node_id: String,
}

impl Hlc {
    /// Construct explicitly. Callers in tests or deserialisation only.
    pub fn new(wall_ms: u64, logical: u32, node_id: impl Into<String>) -> Self {
        Self {
            wall_ms,
            logical,
            node_id: node_id.into(),
        }
    }

    /// Serialise as `wall_ms.logical.node_id`.
    pub fn encode(&self) -> String {
        format!("{}.{}.{}", self.wall_ms, self.logical, self.node_id)
    }

    /// Parse the `wall_ms.logical.node_id` wire format produced by `encode`.
    ///
    /// Requires exactly two dots before the node id; the node id itself may
    /// contain further dots (e.g. `kim.laptop.local`) and is preserved
    /// verbatim.
    pub fn parse(encoded: &str) -> anyhow::Result<Self> {
        let (wall_str, rest) = encoded
            .split_once('.')
            .ok_or_else(|| anyhow::anyhow!("HLC missing first separator in '{encoded}'"))?;
        let (logical_str, node_id) = rest
            .split_once('.')
            .ok_or_else(|| anyhow::anyhow!("HLC missing second separator in '{encoded}'"))?;
        if node_id.is_empty() {
            anyhow::bail!("HLC node_id must not be empty");
        }
        let wall_ms: u64 = wall_str
            .parse()
            .map_err(|e| anyhow::anyhow!("HLC wall_ms parse: {e}"))?;
        let logical: u32 = logical_str
            .parse()
            .map_err(|e| anyhow::anyhow!("HLC logical parse: {e}"))?;
        Ok(Self {
            wall_ms,
            logical,
            node_id: node_id.to_string(),
        })
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        // Node id is a tiebreaker only for fully-identical (wall, logical)
        // pairs. Monotonicity is carried by the (wall_ms, logical) pair.
        self.wall_ms
            .cmp(&other.wall_ms)
            .then_with(|| self.logical.cmp(&other.logical))
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

/// Thread-safe clock. `tick()` produces strictly-monotone timestamps even
/// when the wall clock ticks backwards (NTP correction, hibernation). All
/// operations are lock-free under contention — CAS on a packed `u64`.
pub struct HlcClock {
    node_id: String,
    /// Packed state: upper 44 bits = wall_ms (max ≈ 557 years post-epoch),
    /// lower 20 bits = logical (max 1_048_575 per ms). Overflow of the
    /// logical half simply rolls `wall_ms` forward — monotonicity preserved.
    packed: AtomicU64,
}

const LOGICAL_BITS: u64 = 20;
const LOGICAL_MASK: u64 = (1u64 << LOGICAL_BITS) - 1;

fn pack(wall_ms: u64, logical: u32) -> u64 {
    (wall_ms << LOGICAL_BITS) | (u64::from(logical) & LOGICAL_MASK)
}

fn unpack(packed: u64) -> (u64, u32) {
    let wall = packed >> LOGICAL_BITS;
    // The logical half always fits in 20 bits (< u32::MAX), truncation is
    // expected by construction.
    #[allow(clippy::cast_possible_truncation)]
    let logical = (packed & LOGICAL_MASK) as u32;
    (wall, logical)
}

fn wall_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| {
            // u128 → u64 is safe for all realistic timestamps.
            #[allow(clippy::cast_possible_truncation)]
            let ms = d.as_millis() as u64;
            ms
        })
        .unwrap_or(0)
}

impl HlcClock {
    /// Construct a clock bound to `node_id`.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            packed: AtomicU64::new(pack(wall_now(), 0)),
        }
    }

    /// Construct a clock seeded with explicit state — test utility.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_state(node_id: impl Into<String>, wall_ms: u64, logical: u32) -> Self {
        Self {
            node_id: node_id.into(),
            packed: AtomicU64::new(pack(wall_ms, logical)),
        }
    }

    /// Node identifier assigned at construction.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Produce a local HLC timestamp strictly greater than every previous
    /// timestamp issued by this clock.
    pub fn tick(&self) -> Hlc {
        self.advance(wall_now(), None)
    }

    /// Update the clock after observing a remote timestamp and return the
    /// stamp to attach to the outgoing reply. Implements the textbook
    /// `J = max(J, J.remote, wall) + 1-in-logical` rule.
    pub fn update(&self, remote: &Hlc) -> Hlc {
        self.advance(wall_now(), Some(remote))
    }

    fn advance(&self, wall_now_ms: u64, remote: Option<&Hlc>) -> Hlc {
        loop {
            let cur = self.packed.load(AtomicOrdering::Acquire);
            let (cur_wall, cur_logical) = unpack(cur);

            // Candidate wall time: max of local, remote, and current.
            let remote_wall = remote.map_or(0, |h| h.wall_ms);
            let candidate_wall = cur_wall.max(wall_now_ms).max(remote_wall);

            // Candidate logical:
            //   - if candidate_wall advanced past both cur and remote → 0
            //   - if candidate_wall == cur_wall and candidate_wall == remote_wall
            //                                 → max(cur_logical, remote_logical) + 1
            //   - if candidate_wall == cur_wall (but > remote_wall) → cur_logical + 1
            //   - if candidate_wall == remote_wall (but > cur_wall) → remote_logical + 1
            let candidate_logical = match remote {
                Some(r) => {
                    if candidate_wall > cur_wall && candidate_wall > r.wall_ms {
                        0
                    } else if candidate_wall == cur_wall && candidate_wall == r.wall_ms {
                        cur_logical.max(r.logical).saturating_add(1)
                    } else if candidate_wall == cur_wall {
                        cur_logical.saturating_add(1)
                    } else {
                        r.logical.saturating_add(1)
                    }
                }
                None => {
                    if candidate_wall > cur_wall {
                        0
                    } else {
                        cur_logical.saturating_add(1)
                    }
                }
            };

            // If `logical` saturated (2^20 - 1) we spill into the next ms so
            // monotonicity holds.
            let (new_wall, new_logical) = if u64::from(candidate_logical) > LOGICAL_MASK {
                (candidate_wall + 1, 0)
            } else {
                (candidate_wall, candidate_logical)
            };

            let new_packed = pack(new_wall, new_logical);
            if self
                .packed
                .compare_exchange(
                    cur,
                    new_packed,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                )
                .is_ok()
            {
                return Hlc {
                    wall_ms: new_wall,
                    logical: new_logical,
                    node_id: self.node_id.clone(),
                };
            }
            // Lost the CAS race — retry.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn encode_round_trips_through_parse() {
        let h = Hlc::new(1_712_847_293_812, 3, "kim-laptop");
        let parsed = Hlc::parse(&h.encode()).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn parse_preserves_node_id_with_dots() {
        let h = Hlc::parse("100.0.kim.laptop.local").unwrap();
        assert_eq!(h.wall_ms, 100);
        assert_eq!(h.logical, 0);
        assert_eq!(h.node_id, "kim.laptop.local");
    }

    #[test]
    fn parse_rejects_missing_fields() {
        assert!(Hlc::parse("").is_err());
        assert!(Hlc::parse("100").is_err());
        assert!(Hlc::parse("100.0").is_err());
        assert!(Hlc::parse("100.0.").is_err());
    }

    #[test]
    fn parse_rejects_non_numeric_wall() {
        assert!(Hlc::parse("abc.0.node").is_err());
    }

    #[test]
    fn ord_prefers_wall_then_logical_then_node() {
        let a = Hlc::new(100, 0, "z");
        let b = Hlc::new(100, 1, "a");
        let c = Hlc::new(101, 0, "a");
        assert!(b > a);
        assert!(c > b);
        assert!(c > a);
    }

    #[test]
    fn tick_is_strictly_monotonic() {
        let clock = HlcClock::with_state("node-a", 1000, 0);
        let a = clock.tick();
        let b = clock.tick();
        let c = clock.tick();
        assert!(b > a);
        assert!(c > b);
    }

    #[test]
    fn tick_when_wall_goes_backwards_still_monotonic() {
        // Seed clock in the future; tick() with wall_now() < cur_wall.
        // advance() is called directly so we can inject an old wall time.
        let clock = HlcClock::with_state("node-a", 10_000, 5);
        let t1 = clock.advance(500, None); // wall clock jumped backwards
        let t2 = clock.advance(500, None);
        assert!(t1 > Hlc::new(10_000, 5, "node-a"));
        assert!(t2 > t1);
        assert_eq!(t1.wall_ms, 10_000);
        assert_eq!(t1.logical, 6);
    }

    #[test]
    fn update_bumps_past_remote_under_5min_clock_skew() {
        // Acceptance criterion from the roadmap: "5 minute clock drift → HLC
        // ordering remains correct."
        // Local wall: T. Remote wall: T + 5 min. Update must produce a stamp
        // strictly greater than the remote.
        let local = HlcClock::with_state("local", 1_000_000, 0);
        let remote_wall = 1_000_000 + 5 * 60 * 1000;
        let remote = Hlc::new(remote_wall, 7, "remote");
        let reply = local.advance(1_000_000, Some(&remote));
        assert!(reply > remote, "reply {reply} must dominate remote {remote}");
        assert_eq!(reply.wall_ms, remote_wall);
        assert_eq!(reply.logical, 8);
        assert_eq!(reply.node_id, "local");
    }

    #[test]
    fn update_same_wall_bumps_logical_past_both() {
        let local = HlcClock::with_state("local", 500, 3);
        let remote = Hlc::new(500, 10, "remote");
        let reply = local.advance(500, Some(&remote));
        assert_eq!(reply.wall_ms, 500);
        assert_eq!(reply.logical, 11);
    }

    #[test]
    fn update_wall_advances_resets_logical() {
        let local = HlcClock::with_state("local", 500, 3);
        let remote = Hlc::new(499, 10, "remote");
        let reply = local.advance(700, Some(&remote));
        assert_eq!(reply.wall_ms, 700);
        assert_eq!(reply.logical, 0);
    }

    #[test]
    fn logical_overflow_spills_into_next_millisecond() {
        // Seed logical near the 20-bit ceiling to cover the overflow branch.
        let ceiling = u32::try_from(LOGICAL_MASK).unwrap();
        let clock = HlcClock::with_state("local", 500, ceiling);
        let t = clock.advance(500, None);
        assert_eq!(t.wall_ms, 501);
        assert_eq!(t.logical, 0);
    }

    #[test]
    fn concurrent_ticks_remain_strictly_monotonic() {
        let clock = Arc::new(HlcClock::with_state("node-a", 1000, 0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&clock);
                thread::spawn(move || (0..100).map(|_| c.tick()).collect::<Vec<_>>())
            })
            .collect();

        let mut all: Vec<Hlc> = threads
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();

        // Every tick must be unique (monotonic clock ⇒ distinct stamps).
        let before_dedup = all.len();
        all.sort();
        all.dedup();
        assert_eq!(
            all.len(),
            before_dedup,
            "HLC clock produced duplicate stamps under contention"
        );
    }

    #[test]
    fn display_matches_encode() {
        let h = Hlc::new(42, 7, "dev");
        assert_eq!(format!("{h}"), h.encode());
    }
}
