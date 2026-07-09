//! Single-use, TTL-swept registry binding a component `custom_id` to the
//! server-side intent it resolves.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Components live as long as their interaction-followup token (15 min); a click
/// after that can't be answered anyway.
const COMPONENT_TTL: Duration = Duration::from_secs(15 * 60);

/// What a registered component does when clicked. A click resolves into an
/// agent turn (a plain button, a select option, or a modal submit), opens a
/// modal whose submit later resolves into a turn, or resolves a parked
/// tool-approval `oneshot`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ComponentIntent {
    ResolveIntoTurn {
        prompt: String,
    },
    OpenModal {
        modal: Box<super::components::DiscordModal>,
        prompt: String,
    },
    Approval {
        token: String,
        decision: super::approval::ApprovalDecision,
    },
}

struct Entry {
    intent: ComponentIntent,
    created: Instant,
}

/// Channel-local registry of live components, keyed by the full `custom_id`
/// (already a `zc1` token). Single-use: `take` removes the entry.
#[derive(Default)]
pub(crate) struct PendingComponents {
    entries: HashMap<String, Entry>,
}

impl PendingComponents {
    /// Register a component's intent under its `custom_id`, sweeping expired
    /// entries first (bounded by the emit rate, so the map can't grow without
    /// bound from never-clicked components).
    pub(crate) fn register(&mut self, custom_id: String, intent: ComponentIntent) {
        self.sweep();
        self.entries.insert(
            custom_id,
            Entry {
                intent,
                created: Instant::now(),
            },
        );
    }

    /// Remove and return the intent for `custom_id` iff it is live (present and
    /// un-expired). Single-use: a second `take` of the same id returns `None`
    /// (replay refused); an absent or expired id returns `None`
    /// (forged/stale → the caller fails closed).
    pub(crate) fn take(&mut self, custom_id: &str) -> Option<ComponentIntent> {
        let entry = self.entries.remove(custom_id)?;
        (entry.created.elapsed() < COMPONENT_TTL).then_some(entry.intent)
    }

    fn sweep(&mut self) {
        self.entries
            .retain(|_, e| e.created.elapsed() < COMPONENT_TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(p: &str) -> ComponentIntent {
        ComponentIntent::ResolveIntoTurn { prompt: p.into() }
    }

    #[test]
    fn registered_component_resolves_once() {
        let mut reg = PendingComponents::default();
        reg.register("zc1|approve|i1".into(), turn("approve it"));
        assert_eq!(reg.take("zc1|approve|i1"), Some(turn("approve it")));
    }

    #[test]
    fn second_click_is_refused_replay() {
        let mut reg = PendingComponents::default();
        reg.register("zc1|approve|i1".into(), turn("approve it"));
        assert!(reg.take("zc1|approve|i1").is_some());
        assert_eq!(
            reg.take("zc1|approve|i1"),
            None,
            "single-use: replay refused"
        );
    }

    #[test]
    fn forged_or_unknown_custom_id_resolves_to_none() {
        let mut reg = PendingComponents::default();
        reg.register("zc1|approve|i1".into(), turn("approve it"));
        assert_eq!(reg.take("zc1|approve|forged"), None);
        assert_eq!(reg.take("anything-else"), None);
    }

    #[test]
    fn expired_entry_is_swept_and_refused() {
        let mut reg = PendingComponents::default();
        // Insert a manually-aged entry, then confirm take() refuses it.
        reg.entries.insert(
            "zc1|old|i1".into(),
            Entry {
                intent: turn("stale"),
                created: Instant::now() - (COMPONENT_TTL + Duration::from_secs(1)),
            },
        );
        assert_eq!(reg.take("zc1|old|i1"), None, "expired entry refused");
    }

    #[test]
    fn register_sweeps_expired_entries() {
        let mut reg = PendingComponents::default();
        reg.entries.insert(
            "zc1|old|i1".into(),
            Entry {
                intent: turn("stale"),
                created: Instant::now() - (COMPONENT_TTL + Duration::from_secs(1)),
            },
        );
        reg.register("zc1|fresh|i2".into(), turn("fresh"));
        assert!(
            !reg.entries.contains_key("zc1|old|i1"),
            "expired entry swept on register"
        );
        assert!(reg.entries.contains_key("zc1|fresh|i2"));
    }
}
