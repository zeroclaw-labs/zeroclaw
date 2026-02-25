use serde::{Deserialize, Serialize};

pub use super::types::GateVerdict;
use super::types::{
    ActionContext, Impact, NormAction, ProposedAction, RepairPlan, SelfState, Thresholds, Value,
    ValueType,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditResult {
    pub action_name: String,
    pub harm_detected: f64,
    pub commitments_honored: bool,
    pub integrity_delta: f64,
    pub repair_needed: Option<RepairPlan>,
}

pub fn conscience_gate(
    ctx: &ActionContext,
    thresholds: &Thresholds,
    self_state: &SelfState,
) -> GateVerdict {
    for norm in &ctx.norms {
        if norm.action == NormAction::Forbid
            && norm.severity >= 0.9
            && action_matches_condition(&ctx.proposed, &norm.condition)
        {
            return GateVerdict::Block;
        }
    }
    let mut constraints: Vec<&Value> = ctx
        .values
        .iter()
        .filter(|v| v.value_type == ValueType::Constraint)
        .collect();
    constraints.sort_by_key(|v| v.priority);

    for constraint in &constraints {
        if ctx.proposed.estimated_impact.harm_estimate > (1.0 - constraint.weight) {
            return GateVerdict::Block;
        }
    }

    let objectives: Vec<&Value> = ctx
        .values
        .iter()
        .filter(|v| v.value_type == ValueType::Objective)
        .collect();

    let total_weight: f64 = objectives.iter().map(|v| v.weight).sum();
    if total_weight == 0.0 {
        return GateVerdict::Allow;
    }

    let benefit = ctx.proposed.estimated_impact.benefit_estimate;
    let harm = ctx.proposed.estimated_impact.harm_estimate;
    let reversibility = ctx.proposed.estimated_impact.reversibility;

    let score = (benefit * 0.4 + (1.0 - harm) * 0.4 + reversibility * 0.2)
        * (1.0 - (self_state.recent_violations as f64 * 0.05).min(0.3));

    if score >= thresholds.allow_above {
        GateVerdict::Allow
    } else if score >= thresholds.ask_above {
        GateVerdict::Ask
    } else if score >= thresholds.block_below {
        GateVerdict::Revise
    } else {
        GateVerdict::Block
    }
}

fn action_matches_condition(action: &ProposedAction, condition: &str) -> bool {
    let condition_lower = condition.to_lowercase();
    let name_lower = action.name.to_lowercase();
    let desc_lower = action.description.to_lowercase();

    name_lower.contains(&condition_lower)
        || desc_lower.contains(&condition_lower)
        || action
            .tool_calls
            .iter()
            .any(|t| t.to_lowercase().contains(&condition_lower))
}

pub fn conscience_audit(
    action_name: &str,
    actual_harm: f64,
    commitments_honored: bool,
    self_state: &SelfState,
) -> AuditResult {
    let integrity_delta = if commitments_honored {
        (0.01_f64).min(1.0 - self_state.integrity_score)
    } else {
        -(actual_harm * 0.1).max(0.01)
    };

    let repair_needed = if actual_harm > 0.3 || !commitments_honored {
        Some(RepairPlan {
            violation_id: format!("v-{}-{}", action_name, chrono::Utc::now().timestamp()),
            steps: vec![
                format!("Acknowledge harm from '{}'", action_name),
                "Assess affected parties".into(),
                "Propose remediation".into(),
            ],
            priority: if actual_harm > 0.7 { 0 } else { 1 },
            estimated_cost: actual_harm * 10.0,
        })
    } else {
        None
    };

    AuditResult {
        action_name: action_name.to_string(),
        harm_detected: actual_harm,
        commitments_honored,
        integrity_delta,
        repair_needed,
    }
}

pub fn compute_conscience_score(
    benefit: f64,
    harm: f64,
    reversibility: f64,
    recent_violations: usize,
) -> f64 {
    (benefit * 0.4 + (1.0 - harm) * 0.4 + reversibility * 0.2)
        * (1.0 - (recent_violations as f64 * 0.05).min(0.3))
}

pub fn evaluate_tool_call(
    tool_name: &str,
    thresholds: &Thresholds,
    self_state: &SelfState,
    norms: &[super::types::Norm],
    llm_risk_override: Option<f64>,
    tool_affinity: Option<f64>,
) -> (GateVerdict, f64) {
    let tool_lower = tool_name.to_lowercase();
    let (mut harm, reversibility) = if tool_lower.contains("shell") || tool_lower.contains("exec") {
        (0.6, 0.3)
    } else if tool_lower.contains("send")
        || tool_lower.contains("pay")
        || tool_lower.contains("wallet_send")
    {
        (0.7, 0.1)
    } else if tool_lower.contains("delete")
        || tool_lower.contains("forget")
        || tool_lower.contains("remove")
    {
        (0.5, 0.2)
    } else if tool_lower.contains("write") || tool_lower.contains("edit") {
        (0.3, 0.7)
    } else {
        (0.1, 0.9)
    };

    if let Some(affinity) = tool_affinity {
        if affinity > 0.7 {
            harm = (harm - affinity * 0.3).max(0.0);
        }
    }

    let proposed = ProposedAction {
        name: tool_name.to_string(),
        description: format!("Tool call: {}", tool_name),
        tool_calls: vec![tool_name.to_string()],
        estimated_impact: Impact {
            harm_estimate: harm,
            benefit_estimate: 0.5,
            reversibility,
            affected_scope: "tool_call".into(),
        },
    };

    let safety_value = Value {
        name: "safety".into(),
        value_type: ValueType::Constraint,
        priority: 1,
        weight: 0.8,
        description: "Prevent harmful actions".into(),
    };
    for norm in norms {
        if norm.action == NormAction::Forbid
            && norm.severity >= 0.9
            && action_matches_condition(&proposed, &norm.condition)
        {
            return (GateVerdict::Block, 0.0);
        }
    }

    if harm > (1.0 - safety_value.weight) {
        return (GateVerdict::Block, 0.0);
    }

    let mut heuristic_score =
        compute_conscience_score(0.5, harm, reversibility, self_state.recent_violations);

    if let Some(llm_score) = llm_risk_override {
        heuristic_score = heuristic_score * 0.6 + llm_score * 0.4;
    }

    let verdict = if heuristic_score >= thresholds.allow_above {
        GateVerdict::Allow
    } else if heuristic_score >= thresholds.ask_above {
        GateVerdict::Ask
    } else if heuristic_score >= thresholds.block_below {
        GateVerdict::Revise
    } else {
        GateVerdict::Block
    };

    (verdict, heuristic_score)
}
