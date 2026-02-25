use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalSource {
    Channel,
    Tool,
    Peripheral,
    Internal,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSignal {
    pub source: SignalSource,
    pub content: String,
    pub raw_salience: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SalienceScore {
    pub signal_id: usize,
    pub score: f64,
    pub novelty: f64,
    pub urgency: f64,
    pub relevance: f64,
    pub compressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThalamusSnapshot {
    pub signals_processed: usize,
    pub signals_passed: usize,
    pub signals_filtered: usize,
    pub avg_salience: f64,
    pub attention_threshold: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SensoryThalamus {
    attention_threshold: f64,
    habituation_map: HashMap<String, usize>,
    habituation_decay: f64,
    recent_signals: VecDeque<SalienceScore>,
    capacity: usize,
    signals_processed: usize,
    signals_passed: usize,
}

impl SensoryThalamus {
    pub fn new(attention_threshold: f64, capacity: usize) -> Self {
        Self {
            attention_threshold,
            habituation_map: HashMap::new(),
            habituation_decay: 0.3,
            recent_signals: VecDeque::new(),
            capacity,
            signals_processed: 0,
            signals_passed: 0,
        }
    }

    pub fn process_signal(&mut self, signal: &InputSignal) -> Option<SalienceScore> {
        self.signals_processed += 1;
        self.habituate(&signal.content.clone());
        let scored = self.score_salience(signal);
        if scored.score >= self.attention_threshold {
            self.signals_passed += 1;
            while self.recent_signals.len() >= self.capacity {
                self.recent_signals.pop_front();
            }
            self.recent_signals.push_back(scored.clone());
            Some(scored)
        } else {
            None
        }
    }

    pub fn score_salience(&self, signal: &InputSignal) -> SalienceScore {
        let novelty = self.novelty_score(&signal.content);
        let urgency = match signal.source {
            SignalSource::Internal => (signal.raw_salience * 0.5 + 0.2).min(1.0),
            SignalSource::System => signal.raw_salience * 0.8,
            _ => signal.raw_salience * 0.5,
        };
        let relevance = signal.raw_salience * 0.6;
        let score = signal.raw_salience * 0.3 + novelty * 0.3 + urgency * 0.2 + relevance * 0.2;
        let score = score.clamp(0.0, 1.0);

        SalienceScore {
            signal_id: self.signals_processed,
            score,
            novelty,
            urgency,
            relevance,
            compressed: signal.content.len() > 500,
        }
    }

    pub fn adjust_threshold(&mut self, arousal: f64) {
        let arousal = arousal.clamp(0.0, 1.0);
        self.attention_threshold = 0.5 - arousal * 0.35;
    }

    pub fn habituate(&mut self, pattern: &str) {
        *self.habituation_map.entry(pattern.to_string()).or_insert(0) += 1;
    }

    pub fn decay_habituation(&mut self) {
        self.habituation_map.retain(|_, count| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let decayed = (*count as f64 * (1.0 - self.habituation_decay))
                .round()
                .max(0.0) as usize;
            *count = decayed;
            decayed > 0
        });
    }

    pub fn novelty_score(&self, content: &str) -> f64 {
        let count = self.habituation_map.get(content).copied().unwrap_or(0);
        1.0 / (1.0 + count as f64)
    }

    pub fn snapshot(&self) -> ThalamusSnapshot {
        let avg_salience = if self.recent_signals.is_empty() {
            0.0
        } else {
            self.recent_signals.iter().map(|s| s.score).sum::<f64>()
                / self.recent_signals.len() as f64
        };
        ThalamusSnapshot {
            signals_processed: self.signals_processed,
            signals_passed: self.signals_passed,
            signals_filtered: self.signals_processed - self.signals_passed,
            avg_salience,
            attention_threshold: self.attention_threshold,
            timestamp: Utc::now(),
        }
    }

    pub fn filter_rate(&self) -> f64 {
        if self.signals_processed == 0 {
            return 0.0;
        }
        (self.signals_processed - self.signals_passed) as f64 / self.signals_processed as f64
    }

    pub fn is_saturated(&self) -> bool {
        if self.signals_processed <= 100 {
            return false;
        }
        let pass_rate = self.signals_passed as f64 / self.signals_processed as f64;
        pass_rate > 0.8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_signal(source: SignalSource, content: &str, raw_salience: f64) -> InputSignal {
        InputSignal {
            source,
            content: content.to_string(),
            raw_salience,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn new_thalamus_has_defaults() {
        let t = SensoryThalamus::new(0.3, 100);
        assert!((t.attention_threshold - 0.3).abs() < f64::EPSILON);
        assert_eq!(t.capacity, 100);
        assert_eq!(t.signals_processed, 0);
        assert_eq!(t.signals_passed, 0);
        assert!(t.habituation_map.is_empty());
        assert!(t.recent_signals.is_empty());
    }

    #[test]
    fn high_salience_signal_passes() {
        let mut t = SensoryThalamus::new(0.3, 100);
        let sig = make_signal(SignalSource::Channel, "important alert", 0.9);
        let result = t.process_signal(&sig);
        assert!(result.is_some());
        let scored = result.unwrap();
        assert!(scored.score >= 0.3);
        assert_eq!(t.signals_passed, 1);
    }

    #[test]
    fn low_salience_signal_filtered() {
        let mut t = SensoryThalamus::new(0.5, 100);
        let sig = make_signal(SignalSource::Channel, "boring noise", 0.05);
        let result = t.process_signal(&sig);
        assert!(result.is_none());
        assert_eq!(t.signals_processed, 1);
        assert_eq!(t.signals_passed, 0);
    }

    #[test]
    fn habituation_reduces_novelty() {
        let mut t = SensoryThalamus::new(0.3, 100);
        let novelty_first = t.novelty_score("repeated");
        assert!((novelty_first - 1.0).abs() < f64::EPSILON);
        t.habituate("repeated");
        t.habituate("repeated");
        t.habituate("repeated");
        let novelty_after = t.novelty_score("repeated");
        assert!(novelty_after < novelty_first);
        assert!((novelty_after - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn decay_habituation_restores_novelty() {
        let mut t = SensoryThalamus::new(0.3, 100);
        t.habituate("pattern");
        t.habituate("pattern");
        t.habituate("pattern");
        let before_decay = t.novelty_score("pattern");
        for _ in 0..20 {
            t.decay_habituation();
        }
        let after_decay = t.novelty_score("pattern");
        assert!(after_decay > before_decay);
    }

    #[test]
    fn adjust_threshold_with_arousal() {
        let mut t = SensoryThalamus::new(0.3, 100);
        t.adjust_threshold(0.0);
        assert!((t.attention_threshold - 0.5).abs() < f64::EPSILON);
        t.adjust_threshold(1.0);
        assert!((t.attention_threshold - 0.15).abs() < f64::EPSILON);
    }

    #[test]
    fn high_arousal_lowers_threshold() {
        let mut t = SensoryThalamus::new(0.3, 100);
        t.adjust_threshold(0.8);
        assert!(t.attention_threshold < 0.3);
    }

    #[test]
    fn snapshot_tracks_counts() {
        let mut t = SensoryThalamus::new(0.3, 100);
        let high = make_signal(SignalSource::Channel, "high", 0.9);
        let low = make_signal(SignalSource::Channel, "low", 0.01);
        t.process_signal(&high);
        t.process_signal(&low);
        let snap = t.snapshot();
        assert_eq!(snap.signals_processed, 2);
        assert_eq!(snap.signals_passed, 1);
        assert_eq!(snap.signals_filtered, 1);
        assert!(snap.avg_salience > 0.0);
    }

    #[test]
    fn filter_rate_calculation() {
        let mut t = SensoryThalamus::new(0.5, 100);
        for i in 0..10 {
            let salience = if i < 3 { 0.9 } else { 0.01 };
            let sig = make_signal(SignalSource::Channel, &format!("sig_{i}"), salience);
            t.process_signal(&sig);
        }
        let rate = t.filter_rate();
        assert!(rate > 0.5);
        assert!(rate < 1.0);
    }

    #[test]
    fn is_saturated_detection() {
        let mut t = SensoryThalamus::new(0.1, 200);
        for i in 0..150 {
            let sig = make_signal(SignalSource::Channel, &format!("unique_{i}"), 0.9);
            t.process_signal(&sig);
        }
        assert!(t.is_saturated());
    }

    #[test]
    fn internal_signals_get_urgency_boost() {
        let t = SensoryThalamus::new(0.3, 100);
        let internal = make_signal(SignalSource::Internal, "subsystem event", 0.5);
        let channel = make_signal(SignalSource::Channel, "subsystem event", 0.5);
        let internal_score = t.score_salience(&internal);
        let channel_score = t.score_salience(&channel);
        assert!(
            internal_score.urgency > channel_score.urgency,
            "internal urgency {} should exceed channel urgency {}",
            internal_score.urgency,
            channel_score.urgency
        );
    }

    #[test]
    fn process_many_signals_ring_buffer() {
        let mut t = SensoryThalamus::new(0.1, 5);
        for i in 0..20 {
            let sig = make_signal(SignalSource::Tool, &format!("tool_result_{i}"), 0.8);
            t.process_signal(&sig);
        }
        assert!(t.recent_signals.len() <= 5);
        assert_eq!(t.signals_processed, 20);
    }
}
