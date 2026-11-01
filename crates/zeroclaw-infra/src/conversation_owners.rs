//! Process registry of conversation-binding owners.
//!
//! A surface that owns conversation histories publishes one
//! [`ConversationBindingOwner`] per agent for as long as it is serving that
//! agent, and holds the returned [`ConversationOwnerLease`]. Internal turns
//! resolve the owner at the moment they load or append, so a restarted
//! surface replaces its predecessor and a stopped surface stops receiving
//! writes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use zeroclaw_api::conversation_binding::{ConversationBindingOwner, ConversationSurface};

type OwnerKey = (ConversationSurface, String);

struct Published {
    generation: u64,
    owner: Arc<dyn ConversationBindingOwner>,
}

static OWNERS: LazyLock<RwLock<HashMap<OwnerKey, Published>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Keeps one published owner registered. Dropping it unregisters that owner
/// unless a newer publication for the same surface and agent replaced it.
#[must_use = "the owner is unregistered as soon as the lease is dropped"]
pub struct ConversationOwnerLease {
    key: OwnerKey,
    generation: u64,
}

impl Drop for ConversationOwnerLease {
    fn drop(&mut self) {
        let mut owners = OWNERS.write().unwrap_or_else(|e| e.into_inner());
        if owners
            .get(&self.key)
            .is_some_and(|published| published.generation == self.generation)
        {
            owners.remove(&self.key);
        }
    }
}

/// Publish `owner` as the conversation owner for `agent_alias` on `surface`,
/// replacing any earlier publication.
pub fn publish_conversation_owner(
    surface: ConversationSurface,
    agent_alias: &str,
    owner: Arc<dyn ConversationBindingOwner>,
) -> ConversationOwnerLease {
    let key = (surface, agent_alias.to_string());
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    OWNERS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone(), Published { generation, owner });
    ConversationOwnerLease { key, generation }
}

/// The owner currently serving `agent_alias` on `surface`, if any.
pub fn conversation_owner(
    surface: ConversationSurface,
    agent_alias: &str,
) -> Option<Arc<dyn ConversationBindingOwner>> {
    OWNERS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(surface, agent_alias.to_string()))
        .map(|published| Arc::clone(&published.owner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::conversation_binding::ConversationBinding;
    use zeroclaw_api::model_provider::ChatMessage;

    struct Named(&'static str);

    impl ConversationBindingOwner for Named {
        fn load(&self, _binding: &ConversationBinding) -> anyhow::Result<Vec<ChatMessage>> {
            Ok(vec![ChatMessage::assistant(self.0)])
        }

        fn append(
            &self,
            _binding: &ConversationBinding,
            _messages: &[ChatMessage],
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn owner_name(agent: &str) -> Option<String> {
        let binding = ConversationBinding {
            surface: ConversationSurface::Channel,
            route: "r".to_string(),
            key: "k".to_string(),
        };
        conversation_owner(ConversationSurface::Channel, agent)
            .map(|owner| owner.load(&binding).unwrap()[0].content.clone())
    }

    #[test]
    fn lease_drop_unregisters_its_owner() {
        let lease = publish_conversation_owner(
            ConversationSurface::Channel,
            "lease-drop",
            Arc::new(Named("first")),
        );
        assert_eq!(owner_name("lease-drop").as_deref(), Some("first"));
        drop(lease);
        assert_eq!(owner_name("lease-drop"), None);
    }

    #[test]
    fn stale_lease_does_not_remove_its_replacement() {
        let stale = publish_conversation_owner(
            ConversationSurface::Channel,
            "replaced",
            Arc::new(Named("old")),
        );
        let current = publish_conversation_owner(
            ConversationSurface::Channel,
            "replaced",
            Arc::new(Named("new")),
        );
        drop(stale);
        assert_eq!(owner_name("replaced").as_deref(), Some("new"));
        drop(current);
        assert_eq!(owner_name("replaced"), None);
    }

    #[test]
    fn owners_are_scoped_per_agent() {
        let _a = publish_conversation_owner(
            ConversationSurface::Channel,
            "scoped-a",
            Arc::new(Named("a")),
        );
        assert_eq!(owner_name("scoped-a").as_deref(), Some("a"));
        assert_eq!(owner_name("scoped-b"), None);
    }
}
