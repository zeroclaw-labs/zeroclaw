//! Token budget management for the tiered memory system.
//!
//! Decides which MTM entries to compress into LTM when the token budget is
//! exceeded, and provides the token estimation function used throughout the
//! tiered system.

/// Estimate token count of a string. Uses ~4 chars per token heuristic.
///
/// Rounds up to the nearest whole token using integer arithmetic.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Lightweight descriptor for an MTM entry used during batch selection.
pub struct MtmEntry {
    pub id: String,
    pub token_count: usize,
    pub day: chrono::NaiveDate,
}

/// Returns the subset of MTM entries (oldest-first) that should be compressed
/// to bring the total under budget. Returns empty vec if no overflow.
///
/// Arguments:
/// - `entries`: all current MTM entries (sorted internally, oldest first)
/// - `current_total`: total tokens across all MTM entries
/// - `budget`: hard token cap
/// - `hysteresis`: buffer — only compress when overflow > hysteresis
/// - `max_batch_days`: max entries to select at once
pub fn select_overflow_batch(
    entries: &[MtmEntry],
    current_total: usize,
    budget: usize,
    hysteresis: usize,
    max_batch_days: usize,
) -> Vec<MtmEntry> {
    let target = budget.saturating_sub(hysteresis);

    // No overflow: current total is within budget (equivalent to target + hysteresis when hysteresis <= budget)
    if current_total <= budget {
        return Vec::new();
    }

    let must_free = current_total.saturating_sub(target);

    // Build a sorted (oldest-first) view of entry references
    let mut refs: Vec<&MtmEntry> = entries.iter().collect();
    refs.sort_by_key(|e| e.day);

    let mut batch = Vec::new();
    let mut freed = 0usize;

    for entry in refs.into_iter().take(max_batch_days) {
        batch.push(MtmEntry {
            id: entry.id.clone(),
            token_count: entry.token_count,
            day: entry.day,
        });
        freed += entry.token_count;
        if freed >= must_free {
            break;
        }
    }

    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_day(n: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 1, n).unwrap()
    }

    #[test]
    fn estimate_tokens_approx_4_chars_per_token() {
        // 400 chars / 4 = 100 tokens
        let text = "a".repeat(400);
        assert_eq!(estimate_tokens(&text), 100);
    }

    #[test]
    fn select_batch_picks_oldest_until_freed() {
        // total=1000, budget=800, hysteresis=100
        // overflow = 1000-800=200, must_free >= 200+100=300
        // oldest entry has 500 tokens -> frees 500 >= 300, done
        let entries = vec![
            MtmEntry {
                id: "old".into(),
                token_count: 500,
                day: make_day(1),
            },
            MtmEntry {
                id: "mid".into(),
                token_count: 300,
                day: make_day(2),
            },
            MtmEntry {
                id: "new".into(),
                token_count: 200,
                day: make_day(3),
            },
        ];
        let batch = select_overflow_batch(&entries, 1000, 800, 100, 7);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, "old");
    }

    #[test]
    fn select_batch_picks_multiple_when_needed() {
        // each 150 tokens, total=450, budget=200, hysteresis=50
        // overflow=250, must_free>=300 -> need 2 entries (2*150=300)
        let entries = vec![
            MtmEntry {
                id: "a".into(),
                token_count: 150,
                day: make_day(1),
            },
            MtmEntry {
                id: "b".into(),
                token_count: 150,
                day: make_day(2),
            },
            MtmEntry {
                id: "c".into(),
                token_count: 150,
                day: make_day(3),
            },
        ];
        let batch = select_overflow_batch(&entries, 450, 200, 50, 7);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].id, "a");
        assert_eq!(batch[1].id, "b");
    }

    #[test]
    fn no_overflow_returns_empty_batch() {
        let entries = vec![MtmEntry {
            id: "x".into(),
            token_count: 100,
            day: make_day(1),
        }];
        let batch = select_overflow_batch(&entries, 100, 2000, 200, 7);
        assert!(batch.is_empty());
    }

    #[test]
    fn max_batch_days_caps_selection() {
        // 10 entries, each 100 tokens, total=1000, budget=100, hysteresis=0
        // must_free=900, but max_batch_days=3 caps us at 3 entries
        let entries: Vec<MtmEntry> = (1..=10)
            .map(|i| MtmEntry {
                id: i.to_string(),
                token_count: 100,
                day: make_day(i),
            })
            .collect();
        let batch = select_overflow_batch(&entries, 1000, 100, 0, 3);
        assert_eq!(batch.len(), 3);
        // Should be the 3 oldest
        assert_eq!(batch[0].id, "1");
        assert_eq!(batch[1].id, "2");
        assert_eq!(batch[2].id, "3");
    }
}
