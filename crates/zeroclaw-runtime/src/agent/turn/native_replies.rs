//! Exact slot provenance for one admitted native Chat operation.

use std::collections::BTreeMap;

use zeroclaw_providers::ChatMessage;

pub(crate) struct NativeReplyTrace {
    /// Occurrences in the retained flat working history.
    pub(crate) raw: BTreeMap<usize, String>,
    /// Occurrences in this round's canonical append-only delta.
    pub(crate) canonical: BTreeMap<usize, String>,
    /// This round's complete display body denotes exactly the tagged reply.
    pub(crate) whole_completion: Option<String>,
    /// Working-history changes that require replay rather than appending the delta.
    pub(crate) rebuild_required: bool,
    admitted: Option<String>,
    scope: Option<String>,
}

impl NativeReplyTrace {
    pub(crate) fn new(raw: BTreeMap<usize, String>, admitted: Option<String>) -> Self {
        let scope = admitted
            .as_deref()
            .and_then(|id| id.split_once(':'))
            .map(|(scope, _)| scope.to_owned());
        Self {
            raw,
            canonical: BTreeMap::new(),
            whole_completion: None,
            rebuild_required: false,
            admitted,
            scope,
        }
    }

    pub(crate) fn begin_round(&mut self) {
        self.canonical.clear();
        self.whole_completion = None;
        self.rebuild_required = false;
    }

    pub(super) fn begin_live_reply(&self) -> Option<LiveReplySegment> {
        self.scope.clone().map(|scope| LiveReplySegment {
            scope,
            open: None,
            mixed: false,
        })
    }

    pub(super) fn record_reply(
        &mut self,
        raw: usize,
        canonical: usize,
        whole: bool,
    ) -> Option<String> {
        self.record_reply_with_live_occurrence(raw, Some(canonical), whole, None)
    }

    pub(super) fn record_reply_with_live_occurrence(
        &mut self,
        raw: usize,
        canonical: Option<usize>,
        whole: bool,
        live_occurrence: Option<String>,
    ) -> Option<String> {
        let occurrence = live_occurrence
            .or_else(|| self.admitted.take())
            .or_else(|| self.scope.as_deref().map(allocate_scoped_occurrence));
        if let Some(occurrence) = occurrence {
            self.raw.insert(raw, occurrence.clone());
            if let Some(canonical) = canonical {
                self.canonical.insert(canonical, occurrence.clone());
            }
            self.whole_completion = whole.then(|| occurrence.clone());
            Some(occurrence)
        } else {
            None
        }
    }

    pub(in crate::agent) fn remove_raw(&mut self, index: usize) {
        self.remap_raw_after_trim(index, 1, 0);
    }

    pub(in crate::agent) fn rewrite_raw(&mut self, index: usize) {
        self.raw.remove(&index);
        self.rebuild_required = true;
    }

    /// Old rows before `start` stay in place. At that slot the history owner
    /// removes a contiguous number of rows and may insert one breadcrumb.
    pub(super) fn remap_raw_after_trim(&mut self, start: usize, dropped: usize, inserted: usize) {
        if dropped == 0 && inserted == 0 {
            return;
        }
        self.rebuild_required = true;
        let end = start + dropped;
        self.raw = std::mem::take(&mut self.raw)
            .into_iter()
            .filter_map(|(index, id)| {
                if index < start {
                    Some((index, id))
                } else if index >= end {
                    Some((index - dropped + inserted, id))
                } else {
                    None
                }
            })
            .collect();
    }

    /// Normalization preserves the order of non-system rows and emits at most
    /// one merged leading system row. Map that structural permutation.
    pub(super) fn normalize_system_slots(&mut self, history: &[ChatMessage]) {
        let mut next = usize::from(
            history
                .iter()
                .any(|row| row.role == "system" && !row.content.is_empty()),
        );
        let mut old = std::mem::take(&mut self.raw);
        for (index, row) in history.iter().enumerate() {
            if row.role == "system" {
                continue;
            }
            self.rebuild_required |= index != next;
            if let Some(id) = old.remove(&index) {
                self.raw.insert(next, id);
            }
            next += 1;
        }
        self.rebuild_required |= next != history.len();
    }
}

fn allocate_scoped_occurrence(scope: &str) -> String {
    format!("{scope}:{}", uuid::Uuid::new_v4().simple())
}

/// Owned by one provider stream. A replacement request never sees this value,
/// and the final row can adopt only the token returned by the accepted stream.
pub(super) struct LiveReplySegment {
    scope: String,
    open: Option<String>,
    mixed: bool,
}

impl LiveReplySegment {
    pub(super) fn forward(&mut self, delta: String) -> zeroclaw_api::agent::TurnEvent {
        let reply_occurrence = self
            .open
            .get_or_insert_with(|| allocate_scoped_occurrence(&self.scope));
        zeroclaw_api::agent::TurnEvent::NativeReplyChunk {
            delta,
            reply_occurrence: reply_occurrence.clone(),
        }
    }

    pub(super) fn tool_boundary(&mut self, prior_response_text: bool) {
        // The provider's final body accumulates text across its whole call.
        // If a tool split two text runs in that body, neither run represents it.
        self.mixed |= self.open.take().is_some() || prior_response_text;
    }

    pub(super) fn complete(self) -> Option<String> {
        if self.mixed { None } else { self.open }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pure_accepted_stream_can_bind_its_provisional_occurrence() {
        use zeroclaw_api::agent::TurnEvent;
        let marker = |event| match event {
            TurnEvent::NativeReplyChunk {
                reply_occurrence, ..
            } => reply_occurrence,
            _ => panic!("a native stream fragment must be provisional"),
        };
        let mut trace = NativeReplyTrace::new(BTreeMap::new(), Some("scope:first".into()));
        let mut mixed = trace.begin_live_reply().unwrap();
        let narration = marker(mixed.forward("same".into()));
        mixed.tool_boundary(true);
        let later = marker(mixed.forward("same".into()));
        assert_ne!(narration, later);
        assert!(mixed.complete().is_none());

        let mut accepted = trace.begin_live_reply().unwrap();
        accepted.tool_boundary(false);
        let before = marker(accepted.forward("same".into()));
        assert_eq!(before, marker(accepted.forward("same".into())));
        assert_ne!(before, narration);
        let promoted = trace
            .record_reply_with_live_occurrence(3, None, true, accepted.complete())
            .unwrap();
        assert_eq!(promoted, before);
        assert_eq!(trace.raw.get(&3), Some(&before));
        assert!(
            trace.canonical.is_empty(),
            "isolated traces have no canonical delta"
        );
        assert_eq!(trace.whole_completion, Some(before));
    }

    #[test]
    fn terminal_producer_tags_only_the_row_it_appends_to_both_buffers() {
        let mut raw = vec![ChatMessage::assistant("same")];
        let mut canonical = vec![ChatMessage::user("question")];
        let mut trace = NativeReplyTrace::new(
            [(0, "scope:prior".into())].into(),
            Some("scope:reply".into()),
        );
        let mut turn = super::super::TurnState::new(&mut raw, Some(&mut canonical), false);
        turn.sync_pending();
        assert_eq!(
            turn.push_terminal_reply("same".into(), true, Some(&mut trace), None)
                .as_deref(),
            Some("scope:reply")
        );
        turn.push_dual(ChatMessage::assistant("not a terminal reply"));
        assert_eq!(raw[0].content, raw[2].content);
        assert_eq!(
            trace.raw,
            [(0, "scope:prior".into()), (2, "scope:reply".into())].into()
        );
        assert_eq!(trace.canonical, [(1, "scope:reply".into())].into());
        assert_eq!(trace.whole_completion.as_deref(), Some("scope:reply"));
    }

    #[test]
    fn raw_slots_follow_multiple_trims_without_rebinding_canonical_delta() {
        let raw = [
            (2, "old".into()),
            (4, "middle".into()),
            (6, "recent".into()),
        ]
        .into();
        let mut trace = NativeReplyTrace::new(raw, Some("scope:first".into()));
        assert_eq!(
            trace.record_reply(8, 1, true).as_deref(),
            Some("scope:first")
        );
        trace.remap_raw_after_trim(1, 2, 1);
        trace.remap_raw_after_trim(2, 2, 0);
        assert_eq!(
            trace.raw,
            [(3, "recent".into()), (5, "scope:first".into())].into()
        );
        assert_eq!(trace.canonical, [(1, "scope:first".into())].into());
        assert!(trace.rebuild_required);
        trace.begin_round();
        assert!(!trace.rebuild_required);
        assert!(trace.canonical.is_empty());
        assert!(trace.whole_completion.is_none());
        let emitted = trace.record_reply(7, 1, false).unwrap();
        let next = trace.canonical.get(&1).unwrap();
        assert!(next.starts_with("scope:") && next != "scope:first");
        assert_eq!(&trace.raw[&7], next);
        assert_eq!(&emitted, next);
        assert_eq!(trace.raw[&5], "scope:first");
        assert!(trace.whole_completion.is_none());
    }
}
