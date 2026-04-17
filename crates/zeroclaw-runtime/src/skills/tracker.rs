// Skill usage tracking across sessions.
//
// Analyzes tool call history to determine which skills were invoked,
// how often, and whether they succeeded. Feeds into the improvement
// pipeline to prioritize which skills to improve.

use std::collections::HashMap;

/// Usage statistics for a single skill across a session.
#[derive(Debug, Clone, Default)]
pub struct SkillUsageStats {
    pub call_count: usize,
    pub tool_names: Vec<String>,
}

/// Tracks skill usage by analyzing tool call names.
///
/// Skill-derived tools follow the naming convention `skill_name__tool_name`
/// (double underscore separator). The tracker parses this to attribute
/// tool calls back to their parent skills.
#[derive(Debug, Default)]
pub struct SkillUsageTracker {
    stats: HashMap<String, SkillUsageStats>,
}

impl SkillUsageTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a tool invocation. If the tool name matches the skill naming
    /// convention (`skill__tool`), it is attributed to the parent skill.
    pub fn record_call(&mut self, tool_name: &str) {
        if let Some(skill_name) = extract_skill_name(tool_name) {
            let entry = self.stats.entry(skill_name).or_default();
            entry.call_count += 1;
            let tool = tool_name.to_string();
            if !entry.tool_names.contains(&tool) {
                entry.tool_names.push(tool);
            }
        }
    }

    /// Populate the tracker from a slice of tool call records.
    pub fn record_from_history(&mut self, tool_calls: &[super::creator::ToolCallRecord]) {
        for call in tool_calls {
            self.record_call(&call.name);
        }
    }

    /// Get usage stats for a specific skill.
    pub fn get(&self, skill_name: &str) -> Option<&SkillUsageStats> {
        self.stats.get(skill_name)
    }

    /// All tracked skills and their usage stats.
    pub fn all(&self) -> &HashMap<String, SkillUsageStats> {
        &self.stats
    }

    /// Skills sorted by call count (most-used first).
    pub fn most_used(&self) -> Vec<(&str, &SkillUsageStats)> {
        let mut entries: Vec<_> = self.stats.iter().map(|(k, v)| (k.as_str(), v)).collect();
        entries.sort_by(|a, b| b.1.call_count.cmp(&a.1.call_count));
        entries
    }

    /// Total number of distinct skills used.
    pub fn distinct_skills(&self) -> usize {
        self.stats.len()
    }

    /// Total number of skill tool invocations.
    pub fn total_calls(&self) -> usize {
        self.stats.values().map(|s| s.call_count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.stats.is_empty()
    }
}

/// Extract the parent skill name from a tool name following the
/// `skill_name__tool_name` convention. Returns `None` for non-skill tools.
fn extract_skill_name(tool_name: &str) -> Option<String> {
    let idx = tool_name.find("__")?;
    let skill = &tool_name[..idx];
    if skill.is_empty() {
        return None;
    }
    Some(skill.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_skill_name_from_tool() {
        assert_eq!(
            extract_skill_name("deploy__run_deploy"),
            Some("deploy".into())
        );
        assert_eq!(
            extract_skill_name("my_skill__lint_check"),
            Some("my_skill".into())
        );
    }

    #[test]
    fn extract_skill_name_returns_none_for_builtins() {
        assert_eq!(extract_skill_name("shell"), None);
        assert_eq!(extract_skill_name("read_file"), None);
    }

    #[test]
    fn extract_skill_name_handles_edge_cases() {
        assert_eq!(extract_skill_name("__orphan"), None);
        assert_eq!(extract_skill_name(""), None);
        assert_eq!(extract_skill_name("no_double_underscore"), None);
    }

    #[test]
    fn tracker_records_and_retrieves() {
        let mut tracker = SkillUsageTracker::new();
        tracker.record_call("deploy__run_deploy");
        tracker.record_call("deploy__check_status");
        tracker.record_call("lint__run_lint");

        assert_eq!(tracker.distinct_skills(), 2);
        assert_eq!(tracker.total_calls(), 3);

        let deploy = tracker.get("deploy").unwrap();
        assert_eq!(deploy.call_count, 2);
        assert_eq!(deploy.tool_names.len(), 2);

        let lint = tracker.get("lint").unwrap();
        assert_eq!(lint.call_count, 1);
    }

    #[test]
    fn tracker_ignores_builtin_tools() {
        let mut tracker = SkillUsageTracker::new();
        tracker.record_call("shell");
        tracker.record_call("read_file");
        tracker.record_call("write_file");

        assert!(tracker.is_empty());
        assert_eq!(tracker.total_calls(), 0);
    }

    #[test]
    fn tracker_most_used_ordering() {
        let mut tracker = SkillUsageTracker::new();
        tracker.record_call("a__tool");
        tracker.record_call("b__tool");
        tracker.record_call("b__tool");
        tracker.record_call("c__tool");
        tracker.record_call("c__tool");
        tracker.record_call("c__tool");

        let ranked = tracker.most_used();
        assert_eq!(ranked[0].0, "c");
        assert_eq!(ranked[0].1.call_count, 3);
        assert_eq!(ranked[1].0, "b");
        assert_eq!(ranked[2].0, "a");
    }

    #[test]
    fn tracker_record_from_history() {
        use super::super::creator::ToolCallRecord;

        let calls = vec![
            ToolCallRecord {
                name: "deploy__run".into(),
                args: serde_json::json!({}),
            },
            ToolCallRecord {
                name: "shell".into(),
                args: serde_json::json!({}),
            },
            ToolCallRecord {
                name: "deploy__check".into(),
                args: serde_json::json!({}),
            },
        ];

        let mut tracker = SkillUsageTracker::new();
        tracker.record_from_history(&calls);

        assert_eq!(tracker.distinct_skills(), 1);
        assert_eq!(tracker.total_calls(), 2);
        assert_eq!(tracker.get("deploy").unwrap().call_count, 2);
    }

    #[test]
    fn empty_tracker() {
        let tracker = SkillUsageTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.distinct_skills(), 0);
        assert_eq!(tracker.total_calls(), 0);
        assert!(tracker.most_used().is_empty());
    }
}
