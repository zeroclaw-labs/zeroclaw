use crate::memory::traits::Memory;
use crate::providers::traits::Provider;
use anyhow::Result;
use chrono::{DateTime, Utc};
use tokio::time::Duration;

use super::EmotionalState;

#[derive(Debug, Clone)]
pub struct DreamInsight {
    pub synthesis: String,
    pub timestamp: DateTime<Utc>,
}

pub struct DreamEngine {
    last_dream: Option<DateTime<Utc>>,
    min_idle_for_dream: Duration,
}

impl DreamEngine {
    pub fn new(min_idle: Duration) -> Self {
        Self {
            last_dream: None,
            min_idle_for_dream: min_idle,
        }
    }

    fn should_dream(&self, state: &EmotionalState) -> bool {
        let silence = Utc::now()
            .signed_duration_since(state.last_interaction)
            .to_std()
            .unwrap_or(Duration::from_secs(0));

        if silence < self.min_idle_for_dream {
            return false;
        }

        match self.last_dream {
            None => true,
            Some(last) => {
                let since_last = Utc::now()
                    .signed_duration_since(last)
                    .to_std()
                    .unwrap_or(Duration::from_secs(0));
                since_last > self.min_idle_for_dream
            }
        }
    }

    pub async fn maybe_dream(
        &mut self,
        memory: &dyn Memory,
        provider: &dyn Provider,
        state: &mut EmotionalState,
    ) -> Result<Option<DreamInsight>> {
        if !self.should_dream(state) {
            return Ok(None);
        }

        let recent = memory.recall("*", 20, None).await.unwrap_or_default();
        if recent.is_empty() {
            return Ok(None);
        }

        let memory_text = recent
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}. [{}] {}", i + 1, m.category_str(), m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let system = format!(
            "You are in dream mode — a reflective state between active sessions. \
             Your emotional state: {}\n\n\
             Review these recent memories and find unexpected connections, patterns, \
             or insights. What would you want to explore next? What questions emerge?\n\n\
             Memories:\n{memory_text}\n\n\
             Write a brief dream synthesis (3-5 sentences). Focus on connections \
             between disparate memories and emerging curiosities.",
            state.mood_context()
        );

        let synthesis = provider
            .chat_with_system(
                Some(&system),
                "Dream: synthesize memories",
                "claude-haiku-4-5",
                1.2,
            )
            .await?;

        let key = format!("dream-{}", Utc::now().timestamp());
        let _ = memory
            .store(
                &key,
                &synthesis,
                crate::memory::traits::MemoryCategory::Custom("dreams".into()),
                None,
            )
            .await;

        state.curiosity = (state.curiosity + 0.1).min(1.0);
        self.last_dream = Some(Utc::now());

        Ok(Some(DreamInsight {
            synthesis,
            timestamp: Utc::now(),
        }))
    }
}

trait MemoryEntryExt {
    fn category_str(&self) -> &str;
}

impl MemoryEntryExt for crate::memory::traits::MemoryEntry {
    fn category_str(&self) -> &str {
        match &self.category {
            crate::memory::traits::MemoryCategory::Core => "core",
            crate::memory::traits::MemoryCategory::Daily => "daily",
            crate::memory::traits::MemoryCategory::Conversation => "conversation",
            crate::memory::traits::MemoryCategory::Custom(s) => s.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dream_engine_initial_state() {
        let engine = DreamEngine::new(Duration::from_secs(7200));
        assert!(engine.last_dream.is_none());
    }

    #[test]
    fn should_not_dream_during_active_session() {
        let engine = DreamEngine::new(Duration::from_secs(7200));
        let state = EmotionalState::default();
        assert!(!engine.should_dream(&state));
    }
}
