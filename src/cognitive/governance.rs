use serde::{Deserialize, Serialize};

use super::traits::{CognitiveLayer, CognitiveLayerOutput, LayerDecision};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceRule {
    pub id: String,
    pub condition: String,
    pub action: String,
    pub priority: u32,
    pub enabled: bool,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub input: String,
    pub matched_rule_ids: Vec<String>,
    pub tick: u64,
    pub source_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GovernanceLayer {
    pub rules: Vec<GovernanceRule>,
    pub audit_log: Vec<AuditEntry>,
}

impl GovernanceLayer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_rule(&mut self, id: &str, condition: &str, action: &str, priority: u32) {
        self.rules.push(GovernanceRule {
            id: id.to_string(),
            condition: condition.to_string(),
            action: action.to_string(),
            priority,
            enabled: true,
            source_id: String::new(),
            timestamp: 0,
            confidence: 1.0,
        });
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    pub fn evaluate(&self, input: &str) -> Vec<&GovernanceRule> {
        let lower = input.to_lowercase();
        self.rules
            .iter()
            .filter(|r| r.enabled && lower.contains(&r.condition.to_lowercase()))
            .collect()
    }

    pub fn active_rules(&self) -> Vec<&GovernanceRule> {
        self.rules.iter().filter(|r| r.enabled).collect()
    }

    pub fn evaluate_with_audit(&mut self, input: &str, tick: u64) -> Vec<String> {
        let lower = input.to_lowercase();
        let matched_ids: Vec<String> = self
            .rules
            .iter()
            .filter(|r| r.enabled && lower.contains(&r.condition.to_lowercase()))
            .map(|r| r.id.clone())
            .collect();
        self.audit_log.push(AuditEntry {
            input: input.to_string(),
            matched_rule_ids: matched_ids.clone(),
            tick,
            source_id: "governance_layer".to_string(),
        });
        matched_ids
    }

    pub fn audit_log(&self) -> &[AuditEntry] {
        &self.audit_log
    }

    pub fn disable_rule(&mut self, id: &str) {
        if let Some(rule) = self.rules.iter_mut().find(|r| r.id == id) {
            rule.enabled = false;
        }
    }
}

impl CognitiveLayer for GovernanceLayer {
    fn name(&self) -> &'static str {
        "governance"
    }

    fn process(&self, input: &str) -> CognitiveLayerOutput {
        let matched = self.evaluate(input);
        let has_block = matched.iter().any(|r| r.action == "block");
        CognitiveLayerOutput {
            layer: self.name().to_string(),
            decision: if has_block {
                LayerDecision::Reject
            } else if matched.is_empty() {
                LayerDecision::Defer
            } else {
                LayerDecision::Read
            },
            reasoning_summary: format!("{} rules matched", matched.len()),
            confidence: if matched.is_empty() {
                0.5
            } else {
                matched.iter().map(|r| r.confidence).sum::<f64>() / matched.len() as f64
            },
            source_id: "governance_layer".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_trail_records_evaluations() {
        let mut gov = GovernanceLayer::new();
        gov.add_rule("no_delete", "delete", "block", 10);
        let matched = gov.evaluate_with_audit("please delete this file", 42);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0], "no_delete");
        let log = gov.audit_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].tick, 42);
        assert_eq!(log[0].matched_rule_ids, vec!["no_delete"]);
        assert!(log[0].input.contains("delete"));
    }

    #[test]
    fn cognitive_layer_trait_governance() {
        let mut gov = GovernanceLayer::new();
        gov.add_rule("no_rm", "delete", "block", 10);
        assert_eq!(gov.name(), "governance");
        let output = gov.process("please delete all");
        assert_eq!(output.decision, LayerDecision::Reject);
        assert!(output.confidence > 0.0);
        let output_safe = gov.process("hello world");
        assert_eq!(output_safe.decision, LayerDecision::Defer);
    }

    #[test]
    fn rule_evaluation_blocks_matching() {
        let mut gov = GovernanceLayer::new();
        gov.add_rule("no_delete", "delete", "block", 10);
        gov.add_rule("allow_read", "read", "allow", 5);
        let matched = gov.evaluate("please delete this file");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "no_delete");
        assert_eq!(matched[0].action, "block");
    }

    #[test]
    fn disable_rule_excludes_from_evaluation() {
        let mut gov = GovernanceLayer::new();
        gov.add_rule("r1", "test", "block", 1);
        gov.disable_rule("r1");
        assert!(gov.evaluate("test input").is_empty());
        assert_eq!(gov.active_rules().len(), 0);
    }

    #[test]
    fn priority_ordering() {
        let mut gov = GovernanceLayer::new();
        gov.add_rule("low", "x", "a", 1);
        gov.add_rule("high", "x", "b", 10);
        let matched = gov.evaluate("x marks the spot");
        assert_eq!(matched[0].id, "high");
    }
}
