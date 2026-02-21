use crate::config::schema::EstopAutoTriggersConfig;
use crate::security::estop::EstopLevel;
use std::collections::{HashMap, HashSet, VecDeque};

/// Auto-trigger signal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTriggerType {
    FailedGatedAttempts,
    ToolCallRate,
    UnknownDomain,
}

impl AutoTriggerType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailedGatedAttempts => "failed_gated_attempts",
            Self::ToolCallRate => "tool_call_rate",
            Self::UnknownDomain => "unknown_domain",
        }
    }
}

/// Decision emitted by the auto-trigger engine when a threshold is exceeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTriggerDecision {
    pub trigger_type: AutoTriggerType,
    pub threshold: u64,
    pub actual_count: u64,
    pub window_secs: u64,
    pub engaged_level: EstopLevel,
    pub targets: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct SlidingWindowCounter {
    samples: VecDeque<u64>,
}

impl SlidingWindowCounter {
    fn record(&mut self, now_secs: u64, window_secs: u64) -> u64 {
        self.prune(now_secs, window_secs);
        self.samples.push_back(now_secs);
        self.samples.len() as u64
    }

    fn prune(&mut self, now_secs: u64, window_secs: u64) {
        let floor = now_secs.saturating_sub(window_secs.saturating_sub(1));
        while let Some(oldest) = self.samples.front().copied() {
            if oldest < floor {
                let _ = self.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

/// In-memory auto-trigger evaluator.
///
/// Counters are intentionally non-persistent: process restarts reset these
/// transient windows, while estop state remains persisted by `EstopManager`.
#[derive(Debug, Clone)]
pub struct AutoTriggerEngine {
    config: EstopAutoTriggersConfig,
    frozen_tools: Vec<String>,
    failed_gated_attempts: HashMap<String, SlidingWindowCounter>,
    tool_call_rate: SlidingWindowCounter,
    unknown_domains_seen: HashSet<String>,
}

impl AutoTriggerEngine {
    pub fn new(config: EstopAutoTriggersConfig, frozen_tools: Vec<String>) -> Self {
        Self {
            config,
            frozen_tools,
            failed_gated_attempts: HashMap::new(),
            tool_call_rate: SlidingWindowCounter::default(),
            unknown_domains_seen: HashSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn notify_on_auto_trigger(&self) -> bool {
        self.config.notify_on_auto_trigger
    }

    pub fn block_on_unknown_domain(&self) -> bool {
        self.config.block_on_unknown_domain
    }

    pub fn record_failed_gated_attempt(
        &mut self,
        domain: &str,
        now_secs: u64,
    ) -> Option<AutoTriggerDecision> {
        if !self.enabled() {
            return None;
        }

        let normalized = domain.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }

        let threshold = u64::from(self.config.failed_gated_attempts_threshold);
        let window_secs = self.config.failed_gated_attempts_window_secs;

        let counter = self
            .failed_gated_attempts
            .entry(normalized.clone())
            .or_default();
        let count = counter.record(now_secs, window_secs);

        // Trigger when crossing threshold, not on every subsequent sample.
        if count == threshold.saturating_add(1) {
            Some(AutoTriggerDecision {
                trigger_type: AutoTriggerType::FailedGatedAttempts,
                threshold,
                actual_count: count,
                window_secs,
                engaged_level: EstopLevel::DomainBlock(vec![normalized.clone()]),
                targets: vec![normalized],
            })
        } else {
            None
        }
    }

    pub fn record_tool_call(&mut self, now_secs: u64) -> Option<AutoTriggerDecision> {
        if !self.enabled() {
            return None;
        }

        let threshold = u64::from(self.config.tool_call_rate_limit);
        let window_secs = self.config.tool_call_rate_window_secs;
        let count = self.tool_call_rate.record(now_secs, window_secs);
        if count != threshold.saturating_add(1) {
            return None;
        }

        let mut targets = self
            .frozen_tools
            .iter()
            .map(|tool| tool.trim().to_ascii_lowercase())
            .filter(|tool| !tool.is_empty())
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            targets.push("shell".to_string());
        }

        Some(AutoTriggerDecision {
            trigger_type: AutoTriggerType::ToolCallRate,
            threshold,
            actual_count: count,
            window_secs,
            engaged_level: EstopLevel::ToolFreeze(targets.clone()),
            targets,
        })
    }

    pub fn record_unknown_domain(&mut self, domain: &str) -> Option<AutoTriggerDecision> {
        if !self.enabled() || !self.block_on_unknown_domain() {
            return None;
        }

        let normalized = domain.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        if !self.unknown_domains_seen.insert(normalized.clone()) {
            return None;
        }

        Some(AutoTriggerDecision {
            trigger_type: AutoTriggerType::UnknownDomain,
            threshold: 1,
            actual_count: 1,
            window_secs: 0,
            engaged_level: EstopLevel::NetworkKill,
            targets: vec![normalized],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EstopAutoTriggersConfig {
        EstopAutoTriggersConfig {
            enabled: true,
            failed_gated_attempts_threshold: 2,
            failed_gated_attempts_window_secs: 60,
            tool_call_rate_limit: 3,
            tool_call_rate_window_secs: 60,
            block_on_unknown_domain: true,
            notify_on_auto_trigger: true,
        }
    }

    #[test]
    fn failed_gated_attempts_trigger_once_per_threshold_crossing() {
        let mut engine = AutoTriggerEngine::new(test_config(), vec!["shell".into()]);
        assert!(engine
            .record_failed_gated_attempt("secure.chase.com", 100)
            .is_none());
        assert!(engine
            .record_failed_gated_attempt("secure.chase.com", 101)
            .is_none());
        let decision = engine
            .record_failed_gated_attempt("secure.chase.com", 102)
            .expect("threshold crossing should trigger");
        assert_eq!(decision.trigger_type, AutoTriggerType::FailedGatedAttempts);
        assert_eq!(decision.threshold, 2);
        assert_eq!(decision.actual_count, 3);
        assert_eq!(decision.window_secs, 60);
    }

    #[test]
    fn tool_call_rate_trigger_resets_after_window() {
        let mut engine = AutoTriggerEngine::new(test_config(), vec!["shell".into()]);
        assert!(engine.record_tool_call(10).is_none());
        assert!(engine.record_tool_call(20).is_none());
        assert!(engine.record_tool_call(30).is_none());
        let decision = engine
            .record_tool_call(31)
            .expect("4th call in 60s should trigger");
        assert_eq!(decision.trigger_type, AutoTriggerType::ToolCallRate);
        assert_eq!(decision.actual_count, 4);

        // Outside the prior window, we should need another full crossing.
        assert!(engine.record_tool_call(200).is_none());
    }

    #[test]
    fn unknown_domain_trigger_is_deduplicated_per_domain() {
        let mut engine = AutoTriggerEngine::new(test_config(), vec!["shell".into()]);
        assert!(engine.record_unknown_domain("evil.example").is_some());
        assert!(engine.record_unknown_domain("evil.example").is_none());
        assert!(engine.record_unknown_domain("other.example").is_some());
    }

    #[test]
    fn disabled_auto_trigger_emits_no_decisions() {
        let mut cfg = test_config();
        cfg.enabled = false;
        let mut engine = AutoTriggerEngine::new(cfg, vec!["shell".into()]);
        assert!(engine.record_tool_call(1).is_none());
        assert!(engine.record_failed_gated_attempt("a.com", 1).is_none());
        assert!(engine.record_unknown_domain("b.com").is_none());
    }
}
