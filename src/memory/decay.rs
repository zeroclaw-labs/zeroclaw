//! PR #6 — forgetting-curve decay scoring.
//!
//! Pure functions only — the SqliteMemory side wires up `apply_decay_batch`
//! to run nightly via the existing dream-cycle scheduler. Keeping the math
//! out of the SQL layer makes it independently testable and lets future
//! callers (Qdrant, Postgres, Markdown) reuse the policy.
//!
//! ## Score formula
//!
//! ```text
//! decay_score = ln(recall_count + 1) × exp(-days_since_last_recall / half_life) + floor
//! ```
//!
//! - `recall_count` is the lifetime number of times this memory has been
//!   surfaced — the `ln` shape rewards repeated retrieval but flattens fast
//!   so a single high-traffic memory cannot dominate the cache.
//! - `days_since_last_recall` is wall-clock days since `last_recalled`;
//!   never-recalled memories use `days_since_created` as a proxy.
//! - `half_life` is category-specific (see [`half_life_for`]) — identity
//!   facts use `f32::INFINITY` so they never decay; ephemeral chat decays
//!   in 30 days.
//! - `floor` (default 0.1) keeps the long tail above zero so decay never
//!   silently deletes content; sweepers archive on `decay_score < 0.05`,
//!   which only happens when `floor` is overridden to 0.
//!
//! The function is deterministic, side-effect free, and `f32` throughout —
//! property tests in the same file pin the monotonicity invariants the
//! sweeper relies on.

use crate::memory::traits::MemoryCategory;

/// Floor below which a memory is considered "forgotten" and a sweeper may
/// archive it. Tuned so a single recall (count=1) inside the half-life
/// window keeps the score well above the floor.
pub const ARCHIVE_FLOOR: f32 = 0.05;

/// Default additive floor so freshly-stored never-recalled memories
/// don't immediately fall below `ARCHIVE_FLOOR`.
pub const DEFAULT_FLOOR: f32 = 0.1;

/// Per-category half-life in days. Identity facts are intentionally
/// `INFINITY` — losing "I am a lawyer" to forgetting would defeat the
/// product.
#[must_use]
pub fn half_life_for(category: &MemoryCategory) -> f32 {
    match category {
        MemoryCategory::Core => 365.0,
        MemoryCategory::Daily => 90.0,
        MemoryCategory::Conversation => 30.0,
        MemoryCategory::Custom(name) => match name.as_str() {
            "identity" => f32::INFINITY,
            "work" => 365.0,
            "chat" => 30.0,
            "ephemeral" => 7.0,
            _ => 90.0,
        },
    }
}

/// Compute a single memory's decay score.
///
/// `recall_count` is u32 to match the SQL column type. Returns the floor
/// even when `half_life` is non-finite, never `NaN`.
#[must_use]
pub fn decay_score(
    recall_count: u32,
    days_since_last_recall: f32,
    half_life_days: f32,
    floor: f32,
) -> f32 {
    if !half_life_days.is_finite() {
        // INFINITY half-life → e^0 → 1; identity facts stay at full
        // strength forever.
        return ((recall_count + 1) as f32).ln() + floor;
    }
    if half_life_days <= 0.0 || days_since_last_recall.is_nan() {
        return floor;
    }
    let bumped = (recall_count + 1) as f32;
    let aged = (-days_since_last_recall / half_life_days).exp();
    bumped.ln() * aged + floor
}

/// Convenience: derive the score from a category instead of a raw
/// half-life. Useful at the SQL layer where we only have the category
/// string.
#[must_use]
pub fn decay_score_for_category(
    recall_count: u32,
    days_since_last_recall: f32,
    category: &MemoryCategory,
) -> f32 {
    decay_score(
        recall_count,
        days_since_last_recall,
        half_life_for(category),
        DEFAULT_FLOOR,
    )
}

/// Returns true when a memory should be soft-archived because its score
/// has fallen below [`ARCHIVE_FLOOR`]. Pinned in a separate function so
/// callers can override the threshold for tests / configurable sweeps.
#[must_use]
pub fn should_archive(score: f32, threshold: f32) -> bool {
    score.is_finite() && score < threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn fresh_memory_with_one_recall_starts_high() {
        let s = decay_score(1, 0.0, 30.0, DEFAULT_FLOOR);
        // ln(2) × e^0 + 0.1 = 0.693 + 0.1
        assert!(approx(s, 0.793, 1e-3), "got {s}");
    }

    #[test]
    fn never_recalled_memory_today_uses_floor_only() {
        // recall_count=0 → ln(1)=0 → score = 0 + floor.
        let s = decay_score(0, 0.0, 30.0, DEFAULT_FLOOR);
        assert!(approx(s, DEFAULT_FLOOR, 1e-6), "got {s}");
    }

    #[test]
    fn score_decreases_strictly_as_days_grow() {
        let a = decay_score(5, 0.0, 30.0, DEFAULT_FLOOR);
        let b = decay_score(5, 30.0, 30.0, DEFAULT_FLOOR);
        let c = decay_score(5, 90.0, 30.0, DEFAULT_FLOOR);
        assert!(a > b, "{a} !> {b}");
        assert!(b > c, "{b} !> {c}");
    }

    #[test]
    fn score_increases_with_recall_count() {
        let one = decay_score(1, 5.0, 30.0, DEFAULT_FLOOR);
        let many = decay_score(50, 5.0, 30.0, DEFAULT_FLOOR);
        assert!(many > one, "expected {many} > {one}");
    }

    #[test]
    fn identity_category_never_decays_below_recall_floor() {
        let cat = MemoryCategory::Custom("identity".into());
        let s = decay_score_for_category(3, 36500.0, &cat); // 100 years
        // INFINITY half-life → ln(4) + 0.1.
        assert!(s > 1.0, "identity score collapsed: {s}");
    }

    #[test]
    fn ephemeral_category_decays_fastest() {
        let chat = decay_score_for_category(2, 30.0, &MemoryCategory::Conversation);
        let ephemeral = decay_score_for_category(2, 30.0, &MemoryCategory::Custom("ephemeral".into()));
        assert!(
            ephemeral < chat,
            "ephemeral ({ephemeral}) should decay faster than chat ({chat})"
        );
    }

    #[test]
    fn core_category_decays_slower_than_conversation() {
        let core = decay_score_for_category(2, 90.0, &MemoryCategory::Core);
        let convo = decay_score_for_category(2, 90.0, &MemoryCategory::Conversation);
        assert!(core > convo, "core {core} should beat conversation {convo}");
    }

    #[test]
    fn nan_days_returns_floor_safely() {
        let s = decay_score(10, f32::NAN, 30.0, DEFAULT_FLOOR);
        assert_eq!(s, DEFAULT_FLOOR);
    }

    #[test]
    fn zero_half_life_clamps_to_floor() {
        let s = decay_score(99, 0.0, 0.0, DEFAULT_FLOOR);
        assert_eq!(s, DEFAULT_FLOOR);
    }

    #[test]
    fn should_archive_respects_threshold() {
        assert!(should_archive(0.04, ARCHIVE_FLOOR));
        assert!(!should_archive(0.06, ARCHIVE_FLOOR));
        assert!(!should_archive(f32::NAN, ARCHIVE_FLOOR));
        assert!(!should_archive(f32::INFINITY, ARCHIVE_FLOOR));
    }

    #[test]
    fn floor_is_additive_lower_bound() {
        // Even at extreme age, score never goes below floor (for finite hl).
        let s = decay_score(0, 1_000_000.0, 30.0, DEFAULT_FLOOR);
        assert!(s >= DEFAULT_FLOOR - 1e-6, "{s} < floor");
    }

    #[test]
    fn half_life_for_custom_categories_recognises_known_aliases() {
        assert!(half_life_for(&MemoryCategory::Custom("identity".into())).is_infinite());
        assert_eq!(
            half_life_for(&MemoryCategory::Custom("work".into())),
            365.0
        );
        assert_eq!(
            half_life_for(&MemoryCategory::Custom("chat".into())),
            30.0
        );
        assert_eq!(
            half_life_for(&MemoryCategory::Custom("ephemeral".into())),
            7.0
        );
        // Unknown name → falls back to the 90-day default so noise doesn't
        // accidentally keep junk forever.
        assert_eq!(
            half_life_for(&MemoryCategory::Custom("anything-else".into())),
            90.0
        );
    }
}
