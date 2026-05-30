//! Reversible context collapse: projects a summarized view of history
//! without permanently discarding messages.
//!
//! Unlike lossy compaction (which splices summaries in place of original
//! messages), context collapse maintains the full log and generates a
//! projected view on demand. Collapsed regions can expand back when
//! context pressure eases.

use daemonclaw_providers::ChatMessage;

/// A collapsed region in the conversation history.
#[derive(Debug, Clone)]
struct CollapsedRegion {
    /// Start index in the full history (inclusive).
    start: usize,
    /// End index in the full history (exclusive).
    end: usize,
    /// Summary text that replaces the region in the projected view.
    summary: String,
    /// Estimated tokens saved by this collapse.
    tokens_saved: usize,
}

/// Manages reversible context collapse over a conversation history.
pub struct ContextCollapser {
    regions: Vec<CollapsedRegion>,
    protect_last_n: usize,
}

impl ContextCollapser {
    pub fn new(protect_last_n: usize) -> Self {
        Self {
            regions: Vec::new(),
            protect_last_n,
        }
    }

    /// Collapse a region of history, replacing it with a summary in the
    /// projected view. The original messages remain in `history` untouched.
    pub fn collapse_region(&mut self, start: usize, end: usize, summary: String, tokens_saved: usize) {
        if start >= end {
            return;
        }
        // Merge with adjacent/overlapping regions
        self.regions.retain(|r| r.end <= start || r.start >= end);
        self.regions.push(CollapsedRegion {
            start,
            end,
            summary,
            tokens_saved,
        });
        self.regions.sort_by_key(|r| r.start);
    }

    /// Expand (uncollapse) a region, restoring original messages in the view.
    pub fn expand_region(&mut self, start: usize) {
        self.regions.retain(|r| r.start != start);
    }

    /// Total tokens saved by all active collapses.
    pub fn total_tokens_saved(&self) -> usize {
        self.regions.iter().map(|r| r.tokens_saved).sum()
    }

    /// Produce a projected view of history with collapsed regions replaced
    /// by their summaries. System messages and the protected tail are never
    /// collapsed.
    pub fn project(&self, history: &[ChatMessage]) -> Vec<ChatMessage> {
        if self.regions.is_empty() {
            return history.to_vec();
        }

        let protect_start = history.len().saturating_sub(self.protect_last_n);
        let mut projected = Vec::with_capacity(history.len());
        let mut i = 0;

        for region in &self.regions {
            // Skip regions that overlap the protected tail
            if region.start >= protect_start {
                continue;
            }
            let effective_end = region.end.min(protect_start);

            // Copy messages before this region
            while i < region.start && i < history.len() {
                projected.push(history[i].clone());
                i += 1;
            }

            // Insert summary in place of the collapsed region
            projected.push(ChatMessage::assistant(format!(
                "[COLLAPSED — {} messages]\n{}",
                effective_end - region.start,
                region.summary
            )));
            i = effective_end;
        }

        // Copy remaining messages (including protected tail)
        while i < history.len() {
            projected.push(history[i].clone());
            i += 1;
        }

        projected
    }

    /// Check if any collapses are active.
    pub fn has_collapses(&self) -> bool {
        !self.regions.is_empty()
    }
}
