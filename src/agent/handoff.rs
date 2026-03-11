//! Human escalation / handoff protocol for conversational AI agents.
//!
//! Defines the `HandoffProtocol` trait for escalating conversations to human
//! agents, along with trigger types and context packaging.

use crate::agent::conversation::{ConversationState, DetectedIntent, EscalationStatus};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Trigger conditions that can cause an escalation to a human agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTrigger {
    /// The most recent intent confidence was below the configured threshold.
    ConfidenceThreshold { confidence: f64, threshold: f64 },
    /// The user explicitly requested a human agent.
    ExplicitRequest,
    /// The conversation topic matched a configured escalation topic.
    TopicMatch { topic: String },
    /// Sentiment analysis flagged negative user sentiment.
    Sentiment { score: f64 },
}

/// Packaged context sent along with a handoff to a human agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffContext {
    pub conversation_id: String,
    pub channel: String,
    pub language: String,
    pub intent_history: Vec<DetectedIntent>,
    pub context: HashMap<String, String>,
    pub trigger: EscalationTrigger,
    pub turn_count: usize,
}

impl HandoffContext {
    /// Build a handoff context from a conversation state and the trigger.
    pub fn from_conversation(state: &ConversationState, trigger: EscalationTrigger) -> Self {
        Self {
            conversation_id: state.conversation_id.clone(),
            channel: state.channel.clone(),
            language: state.language.clone(),
            intent_history: state.intent_history.clone(),
            context: state.context.clone(),
            trigger,
            turn_count: state.turn_count,
        }
    }
}

/// Trait for implementing handoff protocols. Implementations handle the actual
/// transport/integration with human-agent systems (helpdesk, ticketing, etc.).
#[async_trait]
pub trait HandoffProtocol: Send + Sync {
    /// Escalate the conversation to a human agent.
    async fn escalate(&self, context: HandoffContext) -> Result<()>;

    /// Transfer additional context to an already-escalated conversation.
    async fn transfer_context(
        &self,
        conversation_id: &str,
        additional_context: HashMap<String, String>,
    ) -> Result<()>;

    /// Resume the conversation back to the bot after human handoff.
    async fn resume(&self, conversation_id: &str) -> Result<()>;
}

/// Configurable escalation rules for a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRules {
    /// Minimum intent confidence below which escalation is triggered.
    pub confidence_threshold: f64,
    /// Keywords/phrases that trigger explicit-request escalation.
    pub explicit_request_phrases: Vec<String>,
    /// Topic patterns that trigger topic-match escalation.
    pub escalation_topics: Vec<String>,
    /// Sentiment score below which escalation is triggered (if sentiment
    /// analysis is available). None = sentiment trigger disabled.
    pub sentiment_threshold: Option<f64>,
}

impl Default for EscalationRules {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.3,
            explicit_request_phrases: vec![
                "speak to a human".into(),
                "talk to an agent".into(),
                "human agent".into(),
            ],
            escalation_topics: Vec::new(),
            sentiment_threshold: None,
        }
    }
}

/// Evaluate whether an escalation should be triggered.
pub fn evaluate_escalation(
    rules: &EscalationRules,
    user_message: &str,
    last_intent: Option<&DetectedIntent>,
) -> Option<EscalationTrigger> {
    // Check explicit request phrases.
    let lower_msg = user_message.to_lowercase();
    for phrase in &rules.explicit_request_phrases {
        if lower_msg.contains(&phrase.to_lowercase()) {
            return Some(EscalationTrigger::ExplicitRequest);
        }
    }

    // Check confidence threshold.
    if let Some(intent) = last_intent {
        if intent.confidence < rules.confidence_threshold {
            return Some(EscalationTrigger::ConfidenceThreshold {
                confidence: intent.confidence,
                threshold: rules.confidence_threshold,
            });
        }
    }

    // Check topic matches.
    for topic in &rules.escalation_topics {
        if lower_msg.contains(&topic.to_lowercase()) {
            return Some(EscalationTrigger::TopicMatch {
                topic: topic.clone(),
            });
        }
    }

    None
}

/// Apply the escalation trigger to a conversation state — mark as pending.
pub fn apply_escalation(
    state: &mut ConversationState,
    trigger: EscalationTrigger,
) -> HandoffContext {
    state.escalation_status = EscalationStatus::Pending;
    HandoffContext::from_conversation(state, trigger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_escalation_explicit_request() {
        let rules = EscalationRules::default();
        let trigger = evaluate_escalation(&rules, "I want to speak to a human", None);
        assert!(matches!(trigger, Some(EscalationTrigger::ExplicitRequest)));
    }

    #[test]
    fn evaluate_escalation_confidence_threshold() {
        let rules = EscalationRules {
            confidence_threshold: 0.5,
            ..Default::default()
        };
        let intent = DetectedIntent {
            name: "unknown".into(),
            confidence: 0.2,
        };
        let trigger = evaluate_escalation(&rules, "some message", Some(&intent));
        assert!(matches!(
            trigger,
            Some(EscalationTrigger::ConfidenceThreshold { .. })
        ));
    }

    #[test]
    fn evaluate_escalation_no_trigger() {
        let rules = EscalationRules::default();
        let intent = DetectedIntent {
            name: "greeting".into(),
            confidence: 0.95,
        };
        let trigger = evaluate_escalation(&rules, "hello there", Some(&intent));
        assert!(trigger.is_none());
    }

    #[test]
    fn evaluate_escalation_topic_match() {
        let rules = EscalationRules {
            escalation_topics: vec!["billing".into(), "refund".into()],
            ..Default::default()
        };
        let trigger = evaluate_escalation(&rules, "I need a refund please", None);
        assert!(matches!(
            trigger,
            Some(EscalationTrigger::TopicMatch { .. })
        ));
    }

    #[test]
    fn handoff_context_from_conversation_captures_state() {
        let mut state = ConversationState::new("conv-1".into(), "telegram".into(), "en".into());
        state.record_turn();
        state.context.insert("user_tier".into(), "premium".into());
        state.record_intent(DetectedIntent {
            name: "billing".into(),
            confidence: 0.8,
        });

        let ctx = HandoffContext::from_conversation(&state, EscalationTrigger::ExplicitRequest);
        assert_eq!(ctx.conversation_id, "conv-1");
        assert_eq!(ctx.channel, "telegram");
        assert_eq!(ctx.turn_count, 1);
        assert_eq!(ctx.intent_history.len(), 1);
        assert_eq!(ctx.context.get("user_tier").unwrap(), "premium");
    }

    #[test]
    fn apply_escalation_sets_pending_status() {
        let mut state = ConversationState::new("conv-1".into(), "discord".into(), "en".into());
        let _ctx = apply_escalation(&mut state, EscalationTrigger::ExplicitRequest);
        assert_eq!(state.escalation_status, EscalationStatus::Pending);
    }
}
