//! In-memory `ConversationReference` store.
//!
//! Teams delivers the `serviceUrl` + conversation id pair on every inbound
//! activity; proactive (outbound) sends need them back. The pair exists
//! nowhere else in the codebase, so this map is the source of truth —
//! created here from platform data, not copied from config. MVP keeps it
//! in memory only: after a daemon restart, proactive sends fail until the
//! peer messages the bot again (see `docs/msteams-channel-design.md`).

use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;

/// Everything needed to address a conversation proactively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationReference {
    /// Bot Connector base URL for this conversation (e.g.
    /// `https://smba.trafficmanager.net/teams/`). Never hardcoded; Teams
    /// sends it on every activity.
    pub service_url: String,
    /// Raw conversation id as delivered by Teams, including any
    /// `;messageid=` thread suffix.
    pub conversation_id: String,
    /// `personal`, `groupChat`, or `channel` (absent on some activities).
    pub conversation_type: Option<String>,
}

impl ConversationReference {
    /// Whether this is a personal (1:1) conversation with the bot.
    #[must_use]
    pub fn is_personal(&self) -> bool {
        self.conversation_type.as_deref() == Some("personal")
    }
}

/// Upper bound on stored references. A bot serving thousands of distinct
/// conversations legitimately grows this map; beyond the cap the
/// oldest-recorded conversation is evicted (its next inbound message
/// re-records it).
const MAX_CONVERSATIONS: usize = 4096;

/// Size-capped conversation-reference map keyed by raw conversation id.
#[derive(Default)]
pub struct ConversationStore {
    inner: RwLock<StoreInner>,
}

#[derive(Default)]
struct StoreInner {
    map: HashMap<String, ConversationReference>,
    /// Insertion order for FIFO eviction at the cap.
    order: VecDeque<String>,
}

impl ConversationStore {
    /// Record (or refresh) the reference for its conversation id.
    pub fn record(&self, reference: ConversationReference) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        let key = reference.conversation_id.clone();
        if inner.map.insert(key.clone(), reference).is_none() {
            inner.order.push_back(key);
            while inner.map.len() > MAX_CONVERSATIONS {
                let Some(evicted) = inner.order.pop_front() else {
                    break;
                };
                inner.map.remove(&evicted);
            }
        }
    }

    /// Look up the reference for a raw conversation id.
    #[must_use]
    pub fn get(&self, conversation_id: &str) -> Option<ConversationReference> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| inner.map.get(conversation_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(id: &str) -> ConversationReference {
        ConversationReference {
            service_url: "https://smba.trafficmanager.net/teams/".to_string(),
            conversation_id: id.to_string(),
            conversation_type: Some("personal".to_string()),
        }
    }

    #[test]
    fn record_and_get_roundtrip() {
        let store = ConversationStore::default();
        assert!(store.get("a:1").is_none());
        store.record(reference("a:1"));
        assert_eq!(store.get("a:1").unwrap().conversation_id, "a:1");
    }

    #[test]
    fn rerecord_updates_without_duplicating_order() {
        let store = ConversationStore::default();
        store.record(reference("a:1"));
        let updated = ConversationReference {
            service_url: "https://smba.trafficmanager.net/emea/".to_string(),
            ..reference("a:1")
        };
        store.record(updated.clone());
        assert_eq!(store.get("a:1").unwrap(), updated);
        assert_eq!(store.inner.read().unwrap().order.len(), 1);
    }

    #[test]
    fn eviction_drops_oldest_at_cap() {
        let store = ConversationStore::default();
        for i in 0..=MAX_CONVERSATIONS {
            store.record(reference(&format!("a:{i}")));
        }
        assert!(store.get("a:0").is_none(), "oldest entry must be evicted");
        assert!(store.get(&format!("a:{MAX_CONVERSATIONS}")).is_some());
        assert_eq!(store.inner.read().unwrap().map.len(), MAX_CONVERSATIONS);
    }

    #[test]
    fn is_personal_tracks_conversation_type() {
        assert!(reference("a:1").is_personal());
        let channel = ConversationReference {
            conversation_type: Some("channel".to_string()),
            ..reference("19:x@thread.tacv2")
        };
        assert!(!channel.is_personal());
        let unknown = ConversationReference {
            conversation_type: None,
            ..reference("a:2")
        };
        assert!(!unknown.is_personal());
    }
}
