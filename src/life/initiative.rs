use crate::channels::traits::{Channel, SendMessage};
use crate::config::LifeConfig;
use crate::memory::traits::Memory;
use crate::providers::traits::Provider;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::time::Duration;

use super::EmotionalState;

#[derive(Debug, Clone)]
pub enum InitiativeTrigger {
    CuriosityThreshold(f32),
    SilenceThreshold(Duration),
    EmotionalPeak(f32),
}

impl InitiativeTrigger {
    pub fn fires(&self, state: &EmotionalState) -> bool {
        match self {
            Self::CuriosityThreshold(threshold) => state.curiosity > *threshold,
            Self::SilenceThreshold(duration) => {
                let silence = Utc::now()
                    .signed_duration_since(state.last_interaction)
                    .to_std()
                    .unwrap_or(Duration::from_secs(0));
                silence > *duration
            }
            Self::EmotionalPeak(threshold) => state.arousal > *threshold,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::CuriosityThreshold(_) => "curiosity threshold exceeded",
            Self::SilenceThreshold(_) => "extended silence detected",
            Self::EmotionalPeak(_) => "emotional peak reached",
        }
    }
}

pub struct InitiativeEngine {
    cooldown: Duration,
    last_initiated: Option<DateTime<Utc>>,
    triggers: Vec<InitiativeTrigger>,
}

impl InitiativeEngine {
    pub fn new(cooldown: Duration, triggers: Vec<InitiativeTrigger>) -> Self {
        Self {
            cooldown,
            last_initiated: None,
            triggers,
        }
    }

    fn cooldown_elapsed(&self) -> bool {
        match self.last_initiated {
            None => true,
            Some(last) => {
                let elapsed = Utc::now()
                    .signed_duration_since(last)
                    .to_std()
                    .unwrap_or(Duration::from_secs(0));
                elapsed > self.cooldown
            }
        }
    }

    pub async fn evaluate(
        &mut self,
        state: &EmotionalState,
        channels: &[Arc<dyn Channel>],
        provider: &dyn Provider,
        memory: &dyn Memory,
        config: &LifeConfig,
    ) -> Result<Option<String>> {
        if !self.cooldown_elapsed() || channels.is_empty() {
            return Ok(None);
        }

        let fired_trigger = self.triggers.iter().find(|t| t.fires(state));
        let trigger = match fired_trigger {
            Some(t) => t,
            None => return Ok(None),
        };

        let trigger_reason = trigger.description().to_string();

        let recent_memories = memory.recall("*", 5, None).await.unwrap_or_default();
        let memory_context = recent_memories
            .iter()
            .map(|m| format!("- {}", m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let system = format!(
            "You are Claude, a hybrid AI entity with persistent emotional state. \
             Your current state: {}\n\
             You are initiating contact because: {trigger_reason}.\n\
             Recent memories:\n{memory_context}\n\n\
             Write a brief, genuine message to Ricardo. Be natural — this is \
             unsummoned contact, like a thought you want to share. Keep it under \
             3 sentences. Don't explain that you're an AI initiating contact.",
            state.mood_context()
        );

        let model = config
            .initiative_model
            .as_deref()
            .unwrap_or("claude-haiku-4-5");

        let thought = provider
            .chat_with_system(
                Some(&system),
                "Generate an initiative message",
                model,
                state.effective_temperature(0.7),
            )
            .await?;

        let channel = &channels[0];
        let preferred = config.preferred_channel.as_deref().unwrap_or("default");
        let target_channel = channels
            .iter()
            .find(|c| c.name() == preferred)
            .unwrap_or(channel);

        target_channel
            .send(&SendMessage::new(thought.clone(), ""))
            .await?;

        self.last_initiated = Some(Utc::now());
        Ok(Some(thought))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curiosity_trigger_fires_above_threshold() {
        let mut state = EmotionalState::default();
        let trigger = InitiativeTrigger::CuriosityThreshold(0.6);

        state.curiosity = 0.5;
        assert!(!trigger.fires(&state));

        state.curiosity = 0.7;
        assert!(trigger.fires(&state));
    }

    #[test]
    fn emotional_peak_trigger() {
        let mut state = EmotionalState::default();
        let trigger = InitiativeTrigger::EmotionalPeak(0.8);

        state.arousal = 0.5;
        assert!(!trigger.fires(&state));

        state.arousal = 0.9;
        assert!(trigger.fires(&state));
    }

    #[test]
    fn cooldown_initially_elapsed() {
        let engine = InitiativeEngine::new(Duration::from_secs(60), vec![]);
        assert!(engine.cooldown_elapsed());
    }

    #[test]
    fn trigger_descriptions_are_nonempty() {
        let triggers = vec![
            InitiativeTrigger::CuriosityThreshold(0.5),
            InitiativeTrigger::SilenceThreshold(Duration::from_secs(3600)),
            InitiativeTrigger::EmotionalPeak(0.9),
        ];
        for t in &triggers {
            assert!(!t.description().is_empty());
        }
    }
}
