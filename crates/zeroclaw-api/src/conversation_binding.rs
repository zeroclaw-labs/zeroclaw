//! Conversation binding for internally initiated turns.
//!
//! A binding names one conversation history that an internal turn (a
//! scheduled job, for example) loads as context and appends its completed
//! exchange to. It is runtime-owned state: a surface declares the
//! conversation it is serving through [`crate::TOOL_LOOP_ACTIVE_CONVERSATION`],
//! a dispatching tool may record it, and the surface that owns the history
//! performs every read and write through a [`ConversationBindingOwner`].
//!
//! A binding is correlation, not authority. It never selects a delivery
//! destination, never grants access to a conversation, and is never parsed
//! from message content or tool arguments.

use serde::{Deserialize, Serialize};

use crate::model_provider::ChatMessage;

/// The surface whose history store owns a bound conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSurface {
    /// A channel conversation, keyed by the orchestrator's history key.
    Channel,
}

impl ConversationSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
        }
    }
}

/// The conversation a surface is serving for the current turn.
///
/// Scoped by the surface around the whole turn, so a tool running inside it
/// can record which conversation caused it. `agent_alias` is the agent
/// executing the turn; a consumer must not record a binding for any other
/// agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConversation {
    pub surface: ConversationSurface,
    /// The surface's own name for the route the conversation arrives on (for
    /// a channel, the configured channel the turn was routed by). Histories
    /// of several agents can share one store, so the key alone does not say
    /// whose conversation it is; the route does.
    pub route: String,
    pub key: String,
    pub agent_alias: String,
}

/// A persisted reference to one conversation history.
///
/// The owning agent is not part of the binding: it is whatever agent owns
/// the record carrying the binding, so a rename of that owner keeps the
/// binding reachable. The route is: an owner honours a binding only while it
/// still serves that route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationBinding {
    pub surface: ConversationSurface,
    pub route: String,
    pub key: String,
}

impl ConversationBinding {
    /// The binding an internal job records for `conversation` when it is
    /// created on behalf of `agent_alias`. `None` when the conversation is
    /// being served for a different agent, or its key is blank.
    pub fn for_agent(conversation: &ActiveConversation, agent_alias: &str) -> Option<Self> {
        if conversation.agent_alias != agent_alias || conversation.key.trim().is_empty() {
            return None;
        }
        Some(Self {
            surface: conversation.surface,
            route: conversation.route.clone(),
            key: conversation.key.clone(),
        })
    }

    /// The binding of the conversation the current task is being served in,
    /// for `agent_alias`. `None` outside a turn whose surface declared one,
    /// or when that turn is serving a different agent. This is the only way
    /// a caller learns "which conversation am I in": nothing in a message or
    /// a tool argument can stand in for it.
    pub fn of_current_turn(agent_alias: &str) -> Option<Self> {
        crate::TOOL_LOOP_ACTIVE_CONVERSATION
            .try_with(|conversation| {
                conversation
                    .as_ref()
                    .and_then(|conversation| Self::for_agent(conversation, agent_alias))
            })
            .ok()
            .flatten()
    }
}

/// The serialized reader and writer of one agent's conversations on one
/// surface.
///
/// Implementations must refuse a binding whose route they do not serve, must
/// serialize `append` with every other writer of the same conversation, so
/// concurrent turns interleave without lost updates, and must keep any
/// in-memory copy of the history consistent with what they persist.
pub trait ConversationBindingOwner: Send + Sync {
    /// The conversation's messages, oldest first, in the form the surface
    /// itself sends to a model, without system messages. It may end with a
    /// user message nobody has answered yet. An empty history is
    /// `Ok(vec![])`; an error means the binding is not this owner's to
    /// serve, or the history exists but cannot be read.
    fn load(&self, binding: &ConversationBinding) -> anyhow::Result<Vec<ChatMessage>>;

    /// Append `messages` contiguously, in order, as one serialized write.
    fn append(&self, binding: &ConversationBinding, messages: &[ChatMessage])
    -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(agent: &str, key: &str) -> ActiveConversation {
        ActiveConversation {
            surface: ConversationSurface::Channel,
            route: "telegram".to_string(),
            key: key.to_string(),
            agent_alias: agent.to_string(),
        }
    }

    #[test]
    fn binding_is_recorded_only_for_the_executing_agent() {
        let conversation = active("alpha", "telegram_42_42");
        assert_eq!(
            ConversationBinding::for_agent(&conversation, "alpha"),
            Some(ConversationBinding {
                surface: ConversationSurface::Channel,
                route: "telegram".to_string(),
                key: "telegram_42_42".to_string(),
            })
        );
        assert_eq!(ConversationBinding::for_agent(&conversation, "beta"), None);
    }

    #[test]
    fn current_turn_binding_follows_the_declared_conversation() {
        assert_eq!(ConversationBinding::of_current_turn("alpha"), None);
        let seen = crate::TOOL_LOOP_ACTIVE_CONVERSATION.sync_scope(
            Some(active("alpha", "telegram_42_42")),
            || {
                (
                    ConversationBinding::of_current_turn("alpha"),
                    ConversationBinding::of_current_turn("beta"),
                )
            },
        );
        assert_eq!(
            seen.0.map(|binding| binding.key).as_deref(),
            Some("telegram_42_42")
        );
        assert_eq!(seen.1, None);
        let undeclared = crate::TOOL_LOOP_ACTIVE_CONVERSATION
            .sync_scope(None, || ConversationBinding::of_current_turn("alpha"));
        assert_eq!(undeclared, None);
    }

    #[test]
    fn blank_key_records_no_binding() {
        assert_eq!(
            ConversationBinding::for_agent(&active("alpha", "  "), "alpha"),
            None
        );
    }

    #[test]
    fn binding_round_trips_with_snake_case_surface() {
        let binding = ConversationBinding {
            surface: ConversationSurface::Channel,
            route: "telegram.work".to_string(),
            key: "k".to_string(),
        };
        let json = serde_json::to_string(&binding).unwrap();
        assert_eq!(
            json,
            r#"{"surface":"channel","route":"telegram.work","key":"k"}"#
        );
        assert_eq!(
            serde_json::from_str::<ConversationBinding>(&json).unwrap(),
            binding
        );
    }

    #[test]
    fn unknown_surface_fails_to_deserialize() {
        assert!(
            serde_json::from_str::<ConversationBinding>(
                r#"{"surface":"gateway","route":"r","key":"k"}"#
            )
            .is_err()
        );
    }
}
