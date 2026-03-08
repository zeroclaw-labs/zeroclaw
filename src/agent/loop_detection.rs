use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Sliding-window tool loop detector.
///
/// Tracks the last `max_window` tool calls and detects stalled progress via
/// three tiers: warning, critical, and circuit-breaker.
pub struct ToolLoopDetector {
    history: VecDeque<ToolCallRecord>,
    max_window: usize,
}

struct ToolCallRecord {
    tool_name: String,
    params_hash: u64,
    result_hash: u64,
}

/// Result of a loop-detection check after recording a tool call.
pub enum LoopDetectionResult {
    Ok,
    Warning(String),
    Critical(String),
    CircuitBreaker(String),
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl Default for ToolLoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolLoopDetector {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            max_window: 30,
        }
    }

    /// Record one tool call and its result, then check for loop conditions.
    pub fn record(&mut self, tool_name: &str, params: &str, result: &str) -> LoopDetectionResult {
        let record = ToolCallRecord {
            tool_name: tool_name.to_string(),
            params_hash: hash_str(params),
            result_hash: hash_str(result),
        };
        self.history.push_back(record);
        if self.history.len() > self.max_window {
            self.history.pop_front();
        }
        self.check()
    }

    fn check(&self) -> LoopDetectionResult {
        let ping_pong = self.detect_ping_pong();
        if ping_pong >= 20 {
            return LoopDetectionResult::Critical(format!(
                "Ping-pong loop detected: {ping_pong} alternating tool calls with stable results"
            ));
        }

        let no_progress = self.count_no_progress_tail();

        if no_progress >= 30 {
            return LoopDetectionResult::CircuitBreaker(format!(
                "Circuit breaker triggered: {no_progress} consecutive identical tool calls with no progress"
            ));
        }
        if no_progress >= 20 {
            return LoopDetectionResult::Critical(format!(
                "Loop critical: {no_progress} consecutive identical tool calls with no progress"
            ));
        }
        if no_progress >= 10 {
            return LoopDetectionResult::Warning(format!(
                "Loop warning: {no_progress} consecutive identical tool calls with no progress"
            ));
        }

        LoopDetectionResult::Ok
    }

    /// Count how many of the most-recent records are identical (same name + params + result).
    fn count_no_progress_tail(&self) -> usize {
        let Some(last) = self.history.back() else {
            return 0;
        };
        self.history
            .iter()
            .rev()
            .take_while(|r| {
                r.tool_name == last.tool_name
                    && r.params_hash == last.params_hash
                    && r.result_hash == last.result_hash
            })
            .count()
    }

    /// Count alternating ping-pong pairs in the history tail.
    ///
    /// Detects an A-B-A-B… pattern where both tool+params+result are stable
    /// across alternating positions.
    fn detect_ping_pong(&self) -> usize {
        let len = self.history.len();
        if len < 4 {
            return 0;
        }
        let records: Vec<_> = self.history.iter().collect();
        // Walk backwards counting alternating pairs.
        let mut count = 0usize;
        // The last two entries form the "B" and "A" of the most recent pair.
        let mut i = len;
        while i >= 4 {
            let b = &records[i - 1];
            let a = &records[i - 2];
            let prev_b = &records[i - 3];
            let prev_a = &records[i - 4];
            let b_stable = b.tool_name == prev_b.tool_name
                && b.params_hash == prev_b.params_hash
                && b.result_hash == prev_b.result_hash;
            let a_stable = a.tool_name == prev_a.tool_name
                && a.params_hash == prev_a.params_hash
                && a.result_hash == prev_a.result_hash;
            let alternating = a.tool_name != b.tool_name;
            if b_stable && a_stable && alternating {
                count += 2;
                i -= 2;
            } else {
                break;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_loop_returns_ok() {
        let mut d = ToolLoopDetector::new();
        for i in 0..5 {
            let r = d.record("tool_a", &format!("params_{i}"), &format!("result_{i}"));
            assert!(matches!(r, LoopDetectionResult::Ok));
        }
    }

    #[test]
    fn warning_at_ten_identical_calls() {
        let mut d = ToolLoopDetector::new();
        let mut last = LoopDetectionResult::Ok;
        for _ in 0..10 {
            last = d.record("tool_a", "params", "result");
        }
        assert!(matches!(last, LoopDetectionResult::Warning(_)));
    }

    #[test]
    fn critical_at_twenty_identical_calls() {
        let mut d = ToolLoopDetector::new();
        let mut last = LoopDetectionResult::Ok;
        for _ in 0..20 {
            last = d.record("tool_a", "params", "result");
        }
        assert!(matches!(last, LoopDetectionResult::Critical(_)));
    }

    #[test]
    fn circuit_breaker_at_thirty_identical_calls() {
        let mut d = ToolLoopDetector::new();
        let mut last = LoopDetectionResult::Ok;
        for _ in 0..30 {
            last = d.record("tool_a", "params", "result");
        }
        assert!(matches!(last, LoopDetectionResult::CircuitBreaker(_)));
    }

    #[test]
    fn ping_pong_critical_at_twenty_alternations() {
        let mut d = ToolLoopDetector::new();
        let mut last = LoopDetectionResult::Ok;
        for _ in 0..20 {
            d.record("tool_a", "p", "r");
            last = d.record("tool_b", "p2", "r2");
        }
        assert!(matches!(last, LoopDetectionResult::Critical(_)));
    }
}
