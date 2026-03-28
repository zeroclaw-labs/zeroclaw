//! Loop detection guardrail for the agent tool-call loop.
//!
//! Monitors a sliding window of recent tool calls and their results to detect
//! three repetitive patterns that indicate the agent is stuck:
//!
//! 1. **Exact repeat** — same tool + args called 3+ times consecutively.
//! 2. **Ping-pong** — two tools alternating (A->B->A->B) for 4+ cycles.
//! 3. **No progress** — same tool called 5+ times with different args but
//!    identical result hash each time.
//!
//! Detection triggers escalating responses: `Warning` -> `Block` -> `Break`.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ── Configuration ────────────────────────────────────────────────

/// Configuration for the loop detector, typically derived from
/// `PacingConfig` fields at the call site.
#[derive(Debug, Clone)]
pub(crate) struct LoopDetectorConfig {
    /// Master switch. When `false`, `record` always returns `Ok`.
    pub enabled: bool,
    /// Number of recent calls retained for pattern analysis.
    pub window_size: usize,
    /// How many consecutive exact-repeat calls before escalation starts.
    pub max_repeats: usize,
}

impl Default for LoopDetectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_size: 20,
            max_repeats: 3,
        }
    }
}

// ── Result enum ──────────────────────────────────────────────────

/// Outcome of a loop-detection check after recording a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopDetectionResult {
    /// No pattern detected — continue normally.
    Ok,
    /// A suspicious pattern was detected; the caller should inject a
    /// system-level nudge message into the conversation.
    Warning(String),
    /// The tool call should be refused (output replaced with an error).
    Block(String),
    /// The agent turn should be terminated immediately.
    Break(String),
}

// ── Internal types ───────────────────────────────────────────────

/// A single recorded tool invocation inside the sliding window.
#[derive(Debug, Clone)]
struct ToolCallRecord {
    /// Tool name.
    name: String,
    /// Hash of the serialised arguments.
    args_hash: u64,
    /// Hash of the tool's output/result.
    result_hash: u64,
}

/// Produce a deterministic hash for a JSON value by recursively sorting
/// object keys before serialisation.  This ensures `{"a":1,"b":2}` and
/// `{"b":2,"a":1}` hash identically.
fn hash_value(value: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    let canonical = serde_json::to_string(&canonicalise(value)).unwrap_or_default();
    canonical.hash(&mut hasher);
    hasher.finish()
}

/// Return a clone of `value` with all object keys sorted recursively.
fn canonicalise(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            let new_map: serde_json::Map<String, serde_json::Value> = sorted
                .into_iter()
                .map(|(k, v)| (k.clone(), canonicalise(v)))
                .collect();
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalise).collect())
        }
        other => other.clone(),
    }
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// ── Detector ─────────────────────────────────────────────────────

/// Stateful loop detector that lives for the duration of a single
/// `run_tool_call_loop` invocation.
pub(crate) struct LoopDetector {
    config: LoopDetectorConfig,
    window: VecDeque<ToolCallRecord>,
}

impl LoopDetector {
    pub fn new(config: LoopDetectorConfig) -> Self {
        Self {
            window: VecDeque::with_capacity(config.window_size),
            config,
        }
    }

    /// Record a completed tool call and check for loop patterns.
    ///
    /// * `name` — tool name (e.g. `"shell"`, `"file_read"`).
    /// * `args` — the arguments JSON value sent to the tool.
    /// * `result` — the tool's textual output.
    pub fn record(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        result: &str,
    ) -> LoopDetectionResult {
        if !self.config.enabled {
            return LoopDetectionResult::Ok;
        }

        let record = ToolCallRecord {
            name: name.to_string(),
            args_hash: hash_value(args),
            result_hash: hash_str(result),
        };

        // Maintain sliding window.
        if self.window.len() >= self.config.window_size {
            self.window.pop_front();
        }
        self.window.push_back(record);

        // Run detectors in escalation order (most severe first).
        if let Some(result) = self.detect_exact_repeat() {
            return result;
        }
        if let Some(result) = self.detect_ping_pong() {
            return result;
        }
        if let Some(result) = self.detect_no_progress() {
            return result;
        }

        LoopDetectionResult::Ok
    }

    /// Pattern 1: Same tool + same args called N+ times consecutively.
    ///
    /// Escalation:
    /// - N == max_repeats     -> Warning
    /// - N == max_repeats + 1 -> Block
    /// - N >= max_repeats + 2 -> Break (circuit breaker)
    fn detect_exact_repeat(&self) -> Option<LoopDetectionResult> {
        let max = self.config.max_repeats;
        if self.window.len() < max {
            return None;
        }

        let last = self.window.back()?;
        let consecutive = self
            .window
            .iter()
            .rev()
            .take_while(|r| r.name == last.name && r.args_hash == last.args_hash)
            .count();

        if consecutive >= max + 2 {
            Some(LoopDetectionResult::Break(format!(
                "Circuit breaker: tool '{}' called {} times consecutively with identical arguments",
                last.name, consecutive
            )))
        } else if consecutive > max {
            Some(LoopDetectionResult::Block(format!(
                "Blocked: tool '{}' called {} times consecutively with identical arguments",
                last.name, consecutive
            )))
        } else if consecutive >= max {
            Some(LoopDetectionResult::Warning(format!(
                "Warning: tool '{}' has been called {} times consecutively with identical arguments. \
                 Try a different approach.",
                last.name, consecutive
            )))
        } else {
            None
        }
    }

    /// Pattern 2: Two tools alternating (A->B->A->B) for 4+ full cycles
    /// (i.e. 8 consecutive entries following the pattern).
    fn detect_ping_pong(&self) -> Option<LoopDetectionResult> {
        const MIN_CYCLES: usize = 4;
        let needed = MIN_CYCLES * 2; // each cycle = 2 calls

        if self.window.len() < needed {
            return None;
        }

        let tail: Vec<&ToolCallRecord> = self.window.iter().rev().take(needed).collect();
        // tail[0] is most recent; pattern: A, B, A, B, ...
        let a_name = &tail[0].name;
        let b_name = &tail[1].name;

        if a_name == b_name {
            return None;
        }

        let is_ping_pong = tail.iter().enumerate().all(|(i, r)| {
            if i % 2 == 0 {
                &r.name == a_name
            } else {
                &r.name == b_name
            }
        });

        if !is_ping_pong {
            return None;
        }

        // Count total alternating length for escalation.
        let mut cycles = MIN_CYCLES;
        let extended: Vec<&ToolCallRecord> = self.window.iter().rev().collect();
        for extra_pair in extended.chunks(2).skip(MIN_CYCLES) {
            if extra_pair.len() == 2
                && &extra_pair[0].name == a_name
                && &extra_pair[1].name == b_name
            {
                cycles += 1;
            } else {
                break;
            }
        }

        if cycles >= MIN_CYCLES + 2 {
            Some(LoopDetectionResult::Break(format!(
                "Circuit breaker: tools '{}' and '{}' have been alternating for {} cycles",
                a_name, b_name, cycles
            )))
        } else if cycles > MIN_CYCLES {
            Some(LoopDetectionResult::Block(format!(
                "Blocked: tools '{}' and '{}' have been alternating for {} cycles",
                a_name, b_name, cycles
            )))
        } else {
            Some(LoopDetectionResult::Warning(format!(
                "Warning: tools '{}' and '{}' appear to be alternating ({} cycles). \
                 Consider a different strategy.",
                a_name, b_name, cycles
            )))
        }
    }

    /// Pattern 3: Same tool called 5+ times (with different args each time)
    /// but producing the exact same result hash every time.
    fn detect_no_progress(&self) -> Option<LoopDetectionResult> {
        const MIN_CALLS: usize = 5;

        if self.window.len() < MIN_CALLS {
            return None;
        }

        let last = self.window.back()?;
        let same_tool_same_result: Vec<&ToolCallRecord> = self
            .window
            .iter()
            .rev()
            .take_while(|r| r.name == last.name && r.result_hash == last.result_hash)
            .collect();

        let count = same_tool_same_result.len();
        if count < MIN_CALLS {
            return None;
        }

        // Verify they have *different* args (otherwise exact_repeat handles it).
        let unique_args: std::collections::HashSet<u64> =
            same_tool_same_result.iter().map(|r| r.args_hash).collect();
        if unique_args.len() < 2 {
            // All same args — this is exact-repeat territory, not no-progress.
            return None;
        }

        if count >= MIN_CALLS + 2 {
            Some(LoopDetectionResult::Break(format!(
                "Circuit breaker: tool '{}' called {} times with different arguments but identical results — no progress",
                last.name, count
            )))
        } else if count > MIN_CALLS {
            Some(LoopDetectionResult::Block(format!(
                "Blocked: tool '{}' called {} times with different arguments but identical results",
                last.name, count
            )))
        } else {
            Some(LoopDetectionResult::Warning(format!(
                "Warning: tool '{}' called {} times with different arguments but identical results. \
                 The current approach may not be making progress.",
                last.name, count
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn default_config() -> LoopDetectorConfig {
        LoopDetectorConfig::default()
    }

    fn config_with_repeats(max_repeats: usize) -> LoopDetectorConfig {
        LoopDetectorConfig {
            enabled: true,
            window_size: 20,
            max_repeats,
        }
    }

    // ── Exact repeat tests ───────────────────────────────────────

    #[test]
    fn exact_repeat_warning_at_threshold() {
        let mut det = LoopDetector::new(config_with_repeats(3));
        let args = json!({"path": "/tmp/foo"});

        assert_eq!(
            det.record("file_read", &args, "contents"),
            LoopDetectionResult::Ok
        );
        assert_eq!(
            det.record("file_read", &args, "contents"),
            LoopDetectionResult::Ok
        );
        // 3rd consecutive = warning
        match det.record("file_read", &args, "contents") {
            LoopDetectionResult::Warning(msg) => {
                assert!(msg.contains("file_read"));
                assert!(msg.contains("3 times"));
            }
            other => panic!("expected Warning, got {other:?}"),
        }
    }

    #[test]
    fn exact_repeat_block_at_threshold_plus_one() {
        let mut det = LoopDetector::new(config_with_repeats(3));
        let args = json!({"cmd": "ls"});

        for _ in 0..3 {
            det.record("shell", &args, "output");
        }
        match det.record("shell", &args, "output") {
            LoopDetectionResult::Block(msg) => {
                assert!(msg.contains("shell"));
                assert!(msg.contains("4 times"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn exact_repeat_break_at_threshold_plus_two() {
        let mut det = LoopDetector::new(config_with_repeats(3));
        let args = json!({"q": "test"});

        for _ in 0..4 {
            det.record("search", &args, "no results");
        }
        match det.record("search", &args, "no results") {
            LoopDetectionResult::Break(msg) => {
                assert!(msg.contains("Circuit breaker"));
                assert!(msg.contains("search"));
            }
            other => panic!("expected Break, got {other:?}"),
        }
    }

    #[test]
    fn exact_repeat_resets_on_different_call() {
        let mut det = LoopDetector::new(config_with_repeats(3));
        let args = json!({"x": 1});

        det.record("tool_a", &args, "r1");
        det.record("tool_a", &args, "r1");
        // Interject a different tool — resets the streak.
        det.record("tool_b", &json!({}), "r2");
        det.record("tool_a", &args, "r1");
        det.record("tool_a", &args, "r1");
        // Only 2 consecutive now, should be Ok.
        assert_eq!(
            det.record("tool_a", &json!({"x": 999}), "r1"),
            LoopDetectionResult::Ok
        );
    }

    // ── Ping-pong tests ──────────────────────────────────────────

    #[test]
    fn ping_pong_warning_at_four_cycles() {
        let mut det = LoopDetector::new(default_config());
        let args = json!({});

        // 4 full cycles = 8 calls: A B A B A B A B
        for i in 0..8 {
            let name = if i % 2 == 0 { "read" } else { "write" };
            let result = det.record(name, &args, &format!("r{i}"));
            if i < 7 {
                assert_eq!(result, LoopDetectionResult::Ok, "iteration {i}");
            } else {
                match result {
                    LoopDetectionResult::Warning(msg) => {
                        assert!(msg.contains("read"));
                        assert!(msg.contains("write"));
                        assert!(msg.contains("4 cycles"));
                    }
                    other => panic!("expected Warning at cycle 4, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn ping_pong_escalates_with_more_cycles() {
        let mut det = LoopDetector::new(default_config());
        let args = json!({});

        // 5 cycles = 10 calls.  The 10th call (completing cycle 5) triggers Block.
        for i in 0..10 {
            let name = if i % 2 == 0 { "fetch" } else { "parse" };
            det.record(name, &args, &format!("r{i}"));
        }
        // 11th call extends to 5.5 cycles; detector still counts 5 full -> Block.
        let r = det.record("fetch", &args, "r10");
        match r {
            LoopDetectionResult::Block(msg) => {
                assert!(msg.contains("fetch"));
                assert!(msg.contains("parse"));
                assert!(msg.contains("5 cycles"));
            }
            other => panic!("expected Block at 5 cycles, got {other:?}"),
        }
    }

    #[test]
    fn ping_pong_not_triggered_for_same_tool() {
        let mut det = LoopDetector::new(default_config());
        let args = json!({});

        // Same tool repeated is not ping-pong.
        for _ in 0..10 {
            det.record("read", &args, "data");
        }
        // The exact_repeat detector fires, not ping_pong.
        // Verify by checking message content doesn't mention "alternating".
        let r = det.record("read", &args, "data");
        if let LoopDetectionResult::Break(msg) | LoopDetectionResult::Block(msg) = r {
            assert!(
                !msg.contains("alternating"),
                "should be exact-repeat, not ping-pong"
            );
        }
    }

    // ── No-progress tests ────────────────────────────────────────

    #[test]
    fn no_progress_warning_at_five_different_args_same_result() {
        let mut det = LoopDetector::new(default_config());

        for i in 0..5 {
            let args = json!({"query": format!("attempt_{i}")});
            let result = det.record("search", &args, "no results found");
            if i < 4 {
                assert_eq!(result, LoopDetectionResult::Ok, "iteration {i}");
            } else {
                match result {
                    LoopDetectionResult::Warning(msg) => {
                        assert!(msg.contains("search"));
                        assert!(msg.contains("identical results"));
                    }
                    other => panic!("expected Warning, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn no_progress_escalates_to_block_and_break() {
        let mut det = LoopDetector::new(default_config());

        // 6 calls with different args, same result.
        for i in 0..6 {
            let args = json!({"q": format!("v{i}")});
            det.record("web_fetch", &args, "timeout");
        }
        // 7th call: count=7 which is >= MIN_CALLS(5)+2 -> Break.
        let r7 = det.record("web_fetch", &json!({"q": "v6"}), "timeout");
        match r7 {
            LoopDetectionResult::Break(msg) => {
                assert!(msg.contains("web_fetch"));
                assert!(msg.contains("7 times"));
                assert!(msg.contains("no progress"));
            }
            other => panic!("expected Break at 7 calls, got {other:?}"),
        }
    }

    #[test]
    fn no_progress_not_triggered_when_results_differ() {
        let mut det = LoopDetector::new(default_config());

        for i in 0..8 {
            let args = json!({"q": format!("v{i}")});
            let result = det.record("search", &args, &format!("result_{i}"));
            assert_eq!(result, LoopDetectionResult::Ok, "iteration {i}");
        }
    }

    #[test]
    fn no_progress_not_triggered_when_all_args_identical() {
        // If args are all the same, exact_repeat should fire, not no_progress.
        let mut det = LoopDetector::new(config_with_repeats(6));
        let args = json!({"q": "same"});

        for _ in 0..5 {
            det.record("search", &args, "no results");
        }
        // 6th call = exact repeat at threshold (max_repeats=6) -> Warning.
        // no_progress requires >=2 unique args, so it must NOT fire.
        let r = det.record("search", &args, "no results");
        match r {
            LoopDetectionResult::Warning(msg) => {
                assert!(
                    msg.contains("identical arguments"),
                    "should be exact-repeat Warning, got: {msg}"
                );
            }
            other => panic!("expected exact-repeat Warning, got {other:?}"),
        }
    }

    // ── Disabled / config tests ──────────────────────────────────

    #[test]
    fn disabled_detector_always_returns_ok() {
        let config = LoopDetectorConfig {
            enabled: false,
            ..Default::default()
        };
        let mut det = LoopDetector::new(config);
        let args = json!({"x": 1});

        for _ in 0..20 {
            assert_eq!(det.record("tool", &args, "same"), LoopDetectionResult::Ok);
        }
    }

    #[test]
    fn window_size_limits_memory() {
        let config = LoopDetectorConfig {
            enabled: true,
            window_size: 5,
            max_repeats: 3,
        };
        let mut det = LoopDetector::new(config);
        let args = json!({"x": 1});

        // Fill window with 5 different tools.
        for i in 0..5 {
            det.record(&format!("tool_{i}"), &args, "result");
        }
        assert_eq!(det.window.len(), 5);

        // Adding one more evicts the oldest.
        det.record("tool_5", &args, "result");
        assert_eq!(det.window.len(), 5);
        assert_eq!(det.window.front().unwrap().name, "tool_1");
    }

    // ── Ping-pong with varying args ─────────────────────────────

    #[test]
    fn ping_pong_detects_alternation_with_varying_args() {
        let mut det = LoopDetector::new(default_config());

        // A->B->A->B with different args each time — ping-pong cares only
        // about tool names, not argument equality.
        for i in 0..8 {
            let name = if i % 2 == 0 { "read" } else { "write" };
            let args = json!({"attempt": i});
            let result = det.record(name, &args, &format!("r{i}"));
            if i < 7 {
                assert_eq!(result, LoopDetectionResult::Ok, "iteration {i}");
            } else {
                match result {
                    LoopDetectionResult::Warning(msg) => {
                        assert!(msg.contains("read"));
                        assert!(msg.contains("write"));
                        assert!(msg.contains("4 cycles"));
                    }
                    other => panic!("expected Warning at cycle 4, got {other:?}"),
                }
            }
        }
    }

    // ── Window eviction test ────────────────────────────────────

    #[test]
    fn window_eviction_prevents_stale_pattern_detection() {
        let config = LoopDetectorConfig {
            enabled: true,
            window_size: 6,
            max_repeats: 3,
        };
        let mut det = LoopDetector::new(config);
        let args = json!({"x": 1});

        // 2 consecutive calls of "tool_a".
        det.record("tool_a", &args, "r");
        det.record("tool_a", &args, "r");

        // Fill the rest of the window with different tools (evicting the
        // first "tool_a" calls as the window is only 6).
        for i in 0..5 {
            det.record(&format!("other_{i}"), &json!({}), "ok");
        }

        // Now "tool_a" again — only 1 consecutive, not 3.
        let r = det.record("tool_a", &args, "r");
        assert_eq!(
            r,
            LoopDetectionResult::Ok,
            "stale entries should be evicted"
        );
    }

    // ── hash_value key-order independence ────────────────────────

    #[test]
    fn hash_value_is_key_order_independent() {
        let a = json!({"alpha": 1, "beta": 2});
        let b = json!({"beta": 2, "alpha": 1});
        assert_eq!(
            hash_value(&a),
            hash_value(&b),
            "hash_value must produce identical hashes regardless of JSON key order"
        );
    }

    #[test]
    fn hash_value_nested_key_order_independent() {
        let a = json!({"outer": {"x": 1, "y": 2}, "z": [1, 2]});
        let b = json!({"z": [1, 2], "outer": {"y": 2, "x": 1}});
        assert_eq!(
            hash_value(&a),
            hash_value(&b),
            "nested objects must also be key-order independent"
        );
    }

    // ── Escalation order tests ───────────────────────────────────

    #[test]
    fn exact_repeat_takes_priority_over_no_progress() {
        // If tool+args are identical, exact_repeat fires before no_progress.
        let mut det = LoopDetector::new(config_with_repeats(3));
        let args = json!({"q": "same"});

        det.record("s", &args, "r");
        det.record("s", &args, "r");
        let r = det.record("s", &args, "r");
        match r {
            LoopDetectionResult::Warning(msg) => {
                assert!(msg.contains("identical arguments"));
            }
            other => panic!("expected exact-repeat Warning, got {other:?}"),
        }
    }
}
