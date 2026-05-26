use std::fmt::Write;

use serde::{Deserialize, Serialize};

use super::traits::{ActionOutcome, PhenomenalState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrativeTheme {
    pub label: String,
    pub evidence_count: u32,
    pub first_seen_tick: u64,
    pub last_seen_tick: u64,
}

pub struct NarrativeEngine {
    themes: Vec<NarrativeTheme>,
    capacity: usize,
}

impl NarrativeEngine {
    pub fn new(capacity: usize) -> Self {
        Self {
            themes: Vec::new(),
            capacity,
        }
    }

    pub fn record_tick(
        &mut self,
        tick: u64,
        coherence: f64,
        outcomes: &[ActionOutcome],
        _phenomenal: PhenomenalState,
        approved_actions: &[String],
    ) {
        let mut labels: Vec<String> = Vec::new();

        if coherence > 0.8 {
            labels.push("stability".to_string());
        }
        if coherence < 0.4 {
            labels.push("turbulence".to_string());
        }

        if !outcomes.is_empty() {
            let success_count = outcomes.iter().filter(|o| o.success).count();
            let rate = success_count as f64 / outcomes.len() as f64;
            if rate > 0.5 {
                labels.push("productive".to_string());
            }
            if rate < 0.3 {
                labels.push("struggling".to_string());
            }
        }

        for action in approved_actions {
            if let Some(prefix) = action.split(':').next() {
                let trimmed = prefix.trim();
                if !trimmed.is_empty() {
                    labels.push(trimmed.to_lowercase());
                }
            }
        }

        for label in labels {
            self.upsert_theme(&label, tick);
        }

        if self.themes.len() > self.capacity {
            self.themes
                .sort_by(|a, b| b.evidence_count.cmp(&a.evidence_count));
            self.themes.truncate(self.capacity);
        }
    }

    pub fn synthesize(&self) -> String {
        if self.themes.is_empty() {
            return String::new();
        }

        let mut sorted = self.themes.clone();
        sorted.sort_by(|a, b| b.evidence_count.cmp(&a.evidence_count));

        let mut parts: Vec<String> = Vec::new();

        let has_stability = sorted.iter().any(|t| t.label == "stability");
        let has_turbulence = sorted.iter().any(|t| t.label == "turbulence");
        let has_productive = sorted.iter().any(|t| t.label == "productive");
        let has_struggling = sorted.iter().any(|t| t.label == "struggling");

        if has_stability && has_turbulence {
            parts.push("system stabilizing after coherence dip".to_string());
        } else if has_stability {
            parts.push("system operating stably".to_string());
        } else if has_turbulence {
            parts.push("system experiencing turbulence".to_string());
        }

        if has_productive {
            parts.push("productive execution pattern".to_string());
        } else if has_struggling {
            parts.push("execution struggling".to_string());
        }

        let action_themes: Vec<&NarrativeTheme> = sorted
            .iter()
            .filter(|t| {
                !matches!(
                    t.label.as_str(),
                    "stability" | "turbulence" | "productive" | "struggling"
                )
            })
            .take(2)
            .collect();

        for theme in &action_themes {
            let mut buf = String::new();
            let _ = write!(buf, "{}-driven strategy emerging", theme.label);
            parts.push(buf);
        }

        if parts.is_empty() {
            return "narrative forming".to_string();
        }

        let mut result = String::new();
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    result.push(first.to_ascii_uppercase());
                    result.extend(chars);
                }
            } else {
                result.push_str("; ");
                result.push_str(part);
            }
        }

        result
    }

    pub fn themes(&self) -> &[NarrativeTheme] {
        &self.themes
    }

    pub fn dominant_theme(&self) -> Option<&NarrativeTheme> {
        self.themes.iter().max_by_key(|t| t.evidence_count)
    }

    fn upsert_theme(&mut self, label: &str, tick: u64) {
        let mut found = false;
        for theme in &mut self.themes {
            if theme.label == label {
                theme.evidence_count += 1;
                theme.last_seen_tick = tick;
                found = true;
                break;
            }
        }
        if !found {
            self.themes.push(NarrativeTheme {
                label: label.to_string(),
                evidence_count: 1,
                first_seen_tick: tick,
                last_seen_tick: tick,
            });
        }
    }
}

impl Default for NarrativeEngine {
    fn default() -> Self {
        Self::new(32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::consciousness::traits::AgentKind;
    use chrono::Utc;

    fn make_outcome(action: &str, success: bool) -> ActionOutcome {
        ActionOutcome {
            agent: AgentKind::Execution,
            proposal_id: 0,
            action: action.to_string(),
            success,
            impact: 0.5,
            learnings: Vec::new(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn record_and_synthesize() {
        let mut engine = NarrativeEngine::new(64);
        let phenomenal = PhenomenalState::default();

        for tick in 0..10 {
            let coherence = if tick < 3 { 0.3 } else { 0.9 };
            let outcomes = vec![
                make_outcome("research: scan topics", true),
                make_outcome("execute: run task", tick % 2 == 0),
            ];
            let actions = vec!["research: scan".to_string(), "execute: run".to_string()];
            engine.record_tick(tick, coherence, &outcomes, phenomenal, &actions);
        }

        assert!(!engine.themes().is_empty());
        let text = engine.synthesize();
        assert!(!text.is_empty());
    }

    #[test]
    fn dominant_theme_tracks_most_frequent() {
        let mut engine = NarrativeEngine::new(64);
        let phenomenal = PhenomenalState::default();

        for tick in 0..5 {
            let outcomes = vec![make_outcome("research: deep scan", true)];
            let actions = vec!["research: deep scan".to_string()];
            engine.record_tick(tick, 0.9, &outcomes, phenomenal, &actions);
        }

        engine.record_tick(
            5,
            0.3,
            &[make_outcome("execute: task", false)],
            phenomenal,
            &["execute: task".to_string()],
        );

        let dominant = engine
            .dominant_theme()
            .expect("should have a dominant theme");

        let mut counts: HashMap<&str, u32> = HashMap::new();
        for theme in engine.themes() {
            counts.insert(&theme.label, theme.evidence_count);
        }
        let max_count = counts.values().max().copied().unwrap();
        assert_eq!(dominant.evidence_count, max_count);
    }
}
