use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsageEntry {
    pub tool_name: String,
    pub call_count: u64,
    pub success_count: u64,
    pub last_used: u64,
    pub source_id: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolUsageLog {
    pub entries: HashMap<String, ToolUsageEntry>,
}

impl ToolUsageLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, tool: &str, success: bool, tick: u64) {
        let entry = self
            .entries
            .entry(tool.to_string())
            .or_insert(ToolUsageEntry {
                tool_name: tool.to_string(),
                call_count: 0,
                success_count: 0,
                last_used: 0,
                source_id: String::new(),
                confidence: 0.0,
            });
        entry.call_count += 1;
        if success {
            entry.success_count += 1;
        }
        entry.last_used = tick;
    }

    pub fn success_rate(&self, tool: &str) -> Option<f64> {
        self.entries.get(tool).map(|e| {
            if e.call_count == 0 {
                0.0
            } else {
                e.success_count as f64 / e.call_count as f64
            }
        })
    }

    pub fn most_used(&self) -> Option<&ToolUsageEntry> {
        self.entries.values().max_by_key(|e| e.call_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_success_rate() {
        let mut log = ToolUsageLog::new();
        log.record("shell", true, 1);
        log.record("shell", true, 2);
        log.record("shell", false, 3);
        let rate = log.success_rate("shell").unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 0.01);
        let most = log.most_used().unwrap();
        assert_eq!(most.tool_name, "shell");
        assert_eq!(most.call_count, 3);
    }
}
