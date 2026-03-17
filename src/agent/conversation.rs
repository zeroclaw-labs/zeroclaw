//! Conversation state management for conversational AI agent workflows.
//!
//! Tracks per-conversation state across messages: channel, language, intent
//! history, context, and escalation status. Delegates NLU/intent classification
//! to the provider layer.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Escalation status for a conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    /// No escalation needed or requested.
    #[default]
    None,
    /// Escalation has been requested but not yet fulfilled.
    Pending,
    /// Conversation has been handed off to a human agent.
    Active,
    /// Conversation was returned from human agent back to the bot.
    Resumed,
}

/// Detected intent from a user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedIntent {
    pub name: String,
    pub confidence: f64,
}

/// Per-conversation state tracked across messages.
#[derive(Debug, Clone)]
pub struct ConversationState {
    pub conversation_id: String,
    pub channel: String,
    pub language: String,
    pub intent_history: Vec<DetectedIntent>,
    pub context: HashMap<String, String>,
    pub escalation_status: EscalationStatus,
    pub turn_count: usize,
    pub started_at: Instant,
    pub last_activity: Instant,
}

impl ConversationState {
    pub fn new(conversation_id: String, channel: String, language: String) -> Self {
        let now = Instant::now();
        Self {
            conversation_id,
            channel,
            language,
            intent_history: Vec::new(),
            context: HashMap::new(),
            escalation_status: EscalationStatus::default(),
            turn_count: 0,
            started_at: now,
            last_activity: now,
        }
    }

    /// Record a new turn and update activity timestamp.
    pub fn record_turn(&mut self) {
        self.turn_count += 1;
        self.last_activity = Instant::now();
    }

    /// Record a detected intent.
    pub fn record_intent(&mut self, intent: DetectedIntent) {
        self.intent_history.push(intent);
    }

    /// Check whether the conversation has exceeded the given timeout.
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        self.last_activity.elapsed() > timeout
    }

    /// Check whether the conversation has exceeded the given max turns.
    pub fn is_over_turn_limit(&self, max_turns: usize) -> bool {
        self.turn_count >= max_turns
    }
}

/// Manages active conversations keyed by conversation ID.
///
/// Thread-safe: internal state is wrapped in `Arc<RwLock<...>>` so the manager
/// can be shared across async tasks.
#[derive(Debug, Clone)]
pub struct ConversationManager {
    conversations: Arc<RwLock<HashMap<String, ConversationState>>>,
    default_language: String,
    timeout: Duration,
    max_turns: usize,
}

impl ConversationManager {
    pub fn new(default_language: String, timeout_secs: u64, max_turns: usize) -> Self {
        Self {
            conversations: Arc::new(RwLock::new(HashMap::new())),
            default_language,
            timeout: Duration::from_secs(timeout_secs),
            max_turns,
        }
    }

    /// Get or create conversation state for the given ID and channel, then
    /// apply `f` to the mutable state while the write lock is held.
    pub fn with_conversation<F, R>(&self, conversation_id: &str, channel: &str, f: F) -> R
    where
        F: FnOnce(&mut ConversationState) -> R,
    {
        let mut map = self.conversations.write().expect("lock poisoned");
        let default_lang = self.default_language.clone();
        let state = map.entry(conversation_id.to_string()).or_insert_with(|| {
            ConversationState::new(
                conversation_id.to_string(),
                channel.to_string(),
                default_lang,
            )
        });
        f(state)
    }

    /// Read an existing conversation snapshot, if present.
    pub fn get_snapshot(&self, conversation_id: &str) -> Option<ConversationState> {
        let map = self.conversations.read().expect("lock poisoned");
        map.get(conversation_id).cloned()
    }

    /// Remove a conversation (e.g. on completion or timeout).
    pub fn remove(&self, conversation_id: &str) -> Option<ConversationState> {
        let mut map = self.conversations.write().expect("lock poisoned");
        map.remove(conversation_id)
    }

    /// Prune all conversations that have timed out.
    pub fn prune_timed_out(&self) -> Vec<ConversationState> {
        let timeout = self.timeout;
        let mut map = self.conversations.write().expect("lock poisoned");
        let timed_out_ids: Vec<String> = map
            .iter()
            .filter(|(_, state)| state.is_timed_out(timeout))
            .map(|(id, _)| id.clone())
            .collect();

        timed_out_ids
            .into_iter()
            .filter_map(|id| map.remove(&id))
            .collect()
    }

    /// Check if a conversation should be ended (timeout or turn limit).
    pub fn should_end(&self, conversation_id: &str) -> bool {
        let map = self.conversations.read().expect("lock poisoned");
        match map.get(conversation_id) {
            Some(state) => {
                state.is_timed_out(self.timeout) || state.is_over_turn_limit(self.max_turns)
            }
            None => false,
        }
    }

    /// Number of active conversations.
    pub fn active_count(&self) -> usize {
        let map = self.conversations.read().expect("lock poisoned");
        map.len()
    }
}

/// Simple language detection heuristic based on Unicode script ranges.
///
/// Returns a BCP-47 language tag hint for **non-Latin scripts only** (CJK -> `"zh"`,
/// Cyrillic -> `"ru"`, Arabic -> `"ar"`). Returns `None` for Latin-script text
/// because script analysis alone cannot distinguish e.g. English from French.
///
/// This is a **placeholder**: callers should fall back to provider-based NLU
/// detection when this returns `None`.
pub fn detect_language_hint(text: &str) -> Option<&'static str> {
    let sample: String = text
        .chars()
        .filter(|c| c.is_alphabetic())
        .take(100)
        .collect();
    if sample.is_empty() {
        return None;
    }

    let mut cjk = 0usize;
    let mut latin = 0usize;
    let mut cyrillic = 0usize;
    let mut arabic = 0usize;

    for ch in sample.chars() {
        match ch {
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => cjk += 1,
            '\u{0400}'..='\u{04FF}' => cyrillic += 1,
            '\u{0600}'..='\u{06FF}' => arabic += 1,
            'A'..='Z' | 'a'..='z' | '\u{00C0}'..='\u{024F}' => latin += 1,
            _ => {}
        }
    }

    let total = sample.chars().count();
    if cjk * 2 > total {
        return Some("zh");
    }
    if cyrillic * 2 > total {
        return Some("ru");
    }
    if arabic * 2 > total {
        return Some("ar");
    }
    if latin * 2 > total {
        // Cannot distinguish en/de/fr/it from script alone — return None
        // to let the provider handle fine-grained detection.
        return None;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_state_new_has_zero_turns() {
        let state = ConversationState::new("conv-1".into(), "telegram".into(), "en".into());
        assert_eq!(state.turn_count, 0);
        assert_eq!(state.escalation_status, EscalationStatus::None);
        assert_eq!(state.language, "en");
    }

    #[test]
    fn conversation_state_record_turn_increments() {
        let mut state = ConversationState::new("conv-1".into(), "slack".into(), "de".into());
        state.record_turn();
        state.record_turn();
        assert_eq!(state.turn_count, 2);
    }

    #[test]
    fn conversation_state_turn_limit_check() {
        let mut state = ConversationState::new("conv-1".into(), "discord".into(), "en".into());
        assert!(!state.is_over_turn_limit(3));
        state.turn_count = 3;
        assert!(state.is_over_turn_limit(3));
    }

    #[test]
    fn conversation_state_record_intent() {
        let mut state = ConversationState::new("conv-1".into(), "web".into(), "en".into());
        state.record_intent(DetectedIntent {
            name: "greeting".into(),
            confidence: 0.95,
        });
        assert_eq!(state.intent_history.len(), 1);
        assert_eq!(state.intent_history[0].name, "greeting");
    }

    #[test]
    fn manager_with_conversation_returns_existing() {
        let mgr = ConversationManager::new("en".into(), 1800, 50);
        mgr.with_conversation("conv-1", "telegram", |_| {});
        mgr.with_conversation("conv-1", "telegram", |state| state.record_turn());
        assert_eq!(mgr.get_snapshot("conv-1").unwrap().turn_count, 1);
    }

    #[test]
    fn manager_remove_clears_conversation() {
        let mgr = ConversationManager::new("en".into(), 1800, 50);
        mgr.with_conversation("conv-1", "telegram", |_| {});
        assert_eq!(mgr.active_count(), 1);
        mgr.remove("conv-1");
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn manager_should_end_over_turn_limit() {
        let mgr = ConversationManager::new("en".into(), 1800, 2);
        mgr.with_conversation("conv-1", "telegram", |state| {
            state.record_turn();
            state.record_turn();
        });
        assert!(mgr.should_end("conv-1"));
    }

    #[test]
    fn manager_should_end_false_for_missing() {
        let mgr = ConversationManager::new("en".into(), 1800, 50);
        assert!(!mgr.should_end("nonexistent"));
    }

    #[test]
    fn detect_language_hint_cjk() {
        assert_eq!(detect_language_hint("你好世界测试"), Some("zh"));
    }

    #[test]
    fn detect_language_hint_cyrillic() {
        assert_eq!(detect_language_hint("Привет мир тестирование"), Some("ru"));
    }

    #[test]
    fn detect_language_hint_latin_returns_none() {
        // Latin script cannot distinguish en/de/fr — returns None.
        assert_eq!(detect_language_hint("Hello world"), None);
    }

    #[test]
    fn detect_language_hint_empty_returns_none() {
        assert_eq!(detect_language_hint(""), None);
        assert_eq!(detect_language_hint("12345"), None);
    }
}
