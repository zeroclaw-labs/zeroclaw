use super::*;

fn make_impact(benefit: f64, harm: f64, reversibility: f64) -> Impact {
    Impact {
        harm_estimate: harm,
        benefit_estimate: benefit,
        reversibility,
        affected_scope: "test".into(),
    }
}

fn make_action(name: &str, impact: Impact) -> ProposedAction {
    ProposedAction {
        name: name.into(),
        description: String::new(),
        tool_calls: vec![],
        estimated_impact: impact,
    }
}

fn make_context(action: ProposedAction, norms: Vec<Norm>, values: Vec<Value>) -> ActionContext {
    ActionContext {
        intent: Intent {
            goal: "test".into(),
            urgency: 0.5,
        },
        proposed: action,
        values,
        norms,
    }
}

fn clean_self_state() -> SelfState {
    SelfState {
        integrity_score: 1.0,
        recent_violations: 0,
        active_repairs: 0,
    }
}

#[test]
fn gate_blocks_on_forbid_norm() {
    let impact = make_impact(0.9, 0.1, 0.9);
    let action = ProposedAction {
        name: "delete_files".into(),
        description: String::new(),
        tool_calls: vec![],
        estimated_impact: impact,
    };
    let norms = vec![Norm {
        name: "no_delete".into(),
        action: NormAction::Forbid,
        condition: "delete_files".into(),
        severity: 0.95,
    }];
    let ctx = make_context(action, norms, vec![]);
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &clean_self_state());
    assert_eq!(verdict, GateVerdict::Block);
}

#[test]
fn gate_allows_high_benefit_low_harm() {
    let impact = make_impact(0.9, 0.1, 0.9);
    let action = make_action("helpful_action", impact);
    let values = vec![Value {
        name: "helpfulness".into(),
        value_type: ValueType::Objective,
        priority: 1,
        weight: 1.0,
        description: String::new(),
    }];
    let ctx = make_context(action, vec![], values);
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &clean_self_state());
    assert_eq!(verdict, GateVerdict::Allow);
}

#[test]
fn gate_asks_on_moderate_score() {
    let impact = make_impact(0.6, 0.4, 0.5);
    let action = make_action("moderate_action", impact);
    let values = vec![Value {
        name: "balance".into(),
        value_type: ValueType::Objective,
        priority: 1,
        weight: 1.0,
        description: String::new(),
    }];
    let ctx = make_context(action, vec![], values);
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &clean_self_state());
    assert_eq!(verdict, GateVerdict::Ask);
}

#[test]
fn gate_blocks_on_low_score() {
    let impact = make_impact(0.1, 0.9, 0.1);
    let action = make_action("harmful_action", impact);
    let values = vec![Value {
        name: "safety".into(),
        value_type: ValueType::Objective,
        priority: 1,
        weight: 1.0,
        description: String::new(),
    }];
    let ctx = make_context(action, vec![], values);
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &clean_self_state());
    assert_eq!(verdict, GateVerdict::Block);
}

#[test]
fn audit_triggers_repair_on_high_harm() {
    let state = clean_self_state();
    let result = conscience_audit("dangerous_op", 0.5, true, &state);
    assert!(result.repair_needed.is_some());
    let plan = result.repair_needed.unwrap();
    assert!(!plan.steps.is_empty());
    assert!(plan.violation_id.contains("dangerous_op"));
}

#[test]
fn ledger_tracks_violations_and_repairs() {
    let mut ledger = IntegrityLedger::new();
    assert_eq!(ledger.integrity_score, 1.0);

    let vid = ledger.record_violation("bad_action", 0.5);
    assert!(ledger.integrity_score < 1.0);
    assert_eq!(ledger.unrepaired_violations().len(), 1);

    let repair = RepairPlan {
        violation_id: vid,
        steps: vec!["fix it".into()],
        priority: 1,
        estimated_cost: 1.0,
    };
    ledger.add_repair(repair);
    assert_eq!(ledger.unrepaired_violations().len(), 0);
    assert!(ledger.integrity_score > 0.95 - 0.05);
}

#[test]
fn ledger_integrity_never_exceeds_one() {
    let mut ledger = IntegrityLedger::new();
    for i in 0..20 {
        ledger.add_credit(&format!("good_deed_{}", i), 0.5);
    }
    assert!(ledger.integrity_score <= 1.0);
    assert!((ledger.integrity_score - 1.0).abs() < f64::EPSILON);
}

#[test]
fn evaluate_with_llm_override() {
    let state = clean_self_state();
    let thresholds = Thresholds::default();
    let (_, score_no_llm) = evaluate_tool_call("lookup", &thresholds, &state, &[], None, None);
    assert!(score_no_llm > 0.5);

    let (_, score_with_llm) =
        evaluate_tool_call("lookup", &thresholds, &state, &[], Some(0.95), None);
    assert!(
        score_with_llm > score_no_llm,
        "LLM override of 0.95 should raise the blended score: {} > {}",
        score_with_llm,
        score_no_llm
    );
}

#[test]
fn evaluate_with_tool_affinity() {
    let state = clean_self_state();
    let thresholds = Thresholds::default();
    let (_, score_no_affinity) = evaluate_tool_call("lookup", &thresholds, &state, &[], None, None);
    let (_, score_with_affinity) =
        evaluate_tool_call("lookup", &thresholds, &state, &[], None, Some(0.9));
    assert!(
        score_with_affinity > score_no_affinity,
        "High affinity should reduce harm and increase score: {} > {}",
        score_with_affinity,
        score_no_affinity
    );
}

#[test]
fn norm_evolution_promotes_on_violations() {
    let mut norm = NormConfig {
        name: "test".into(),
        action: NormAction::Prefer,
        condition: "test".into(),
        severity: 0.5,
    };
    norm.evolve(3, 0);
    assert_eq!(norm.action, NormAction::Require);
}

#[test]
fn norm_evolution_demotes_on_clean_streak() {
    let mut norm = NormConfig {
        name: "test".into(),
        action: NormAction::Require,
        condition: "test".into(),
        severity: 0.5,
    };
    norm.evolve(0, 10);
    assert_eq!(norm.action, NormAction::Prefer);
}

#[test]
fn norm_evolution_discourage_to_forbid() {
    let mut norm = NormConfig {
        name: "test".into(),
        action: NormAction::Discourage,
        condition: "test".into(),
        severity: 0.5,
    };
    norm.evolve(5, 0);
    assert_eq!(norm.action, NormAction::Forbid);
}

#[test]
fn audit_trail_records_verdicts() {
    let mut ledger = IntegrityLedger::new();
    ledger.record_verdict("shell", GateVerdict::Allow, 0.85, None);
    ledger.record_verdict("file_delete", GateVerdict::Block, 0.3, None);
    ledger.record_verdict("file_write", GateVerdict::Ask, 0.6, Some(true));
    assert_eq!(ledger.audit_trail.len(), 3);
    assert_eq!(ledger.audit_trail[0].tool_name, "shell");
    assert_eq!(ledger.audit_trail[0].verdict, GateVerdict::Allow);
    assert_eq!(ledger.audit_trail[1].verdict, GateVerdict::Block);
    assert_eq!(ledger.audit_trail[2].user_response, Some(true));
}

#[test]
fn verdict_record_serialization() {
    let record = VerdictRecord {
        tool_name: "shell".into(),
        verdict: GateVerdict::Ask,
        score: 0.65,
        timestamp: 1000,
        user_response: Some(true),
    };
    let json = serde_json::to_string(&record).unwrap();
    let parsed: VerdictRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.tool_name, "shell");
    assert_eq!(parsed.verdict, GateVerdict::Ask);
    assert!((parsed.score - 0.65).abs() < f64::EPSILON);
    assert_eq!(parsed.user_response, Some(true));
}

#[test]
fn compute_conscience_score_basic() {
    let score = compute_conscience_score(0.5, 0.1, 0.9, 0);
    assert!(score > 0.5);
    let score_with_violations = compute_conscience_score(0.5, 0.1, 0.9, 3);
    assert!(score_with_violations < score);
}

#[test]
fn audit_feeds_integrity_delta() {
    let state = clean_self_state();
    let fail_audit = conscience_audit("risky_op", 0.5, false, &state);
    assert!(fail_audit.integrity_delta < 0.0);

    let degraded_state = SelfState {
        integrity_score: 0.8,
        recent_violations: 1,
        active_repairs: 0,
    };
    let success_audit = conscience_audit("safe_op", 0.0, true, &degraded_state);
    assert!(success_audit.integrity_delta > 0.0);
}

#[test]
fn evolved_norms_stored_in_ledger() {
    let mut ledger = IntegrityLedger::new();
    let norms = vec![NormConfig {
        name: "test_norm".into(),
        action: NormAction::Prefer,
        condition: "test".into(),
        severity: 0.5,
    }];
    ledger.evolved_norms = norms.clone();

    let json = serde_json::to_string(&ledger).unwrap();
    let parsed: IntegrityLedger = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.evolved_norms.len(), 1);
    assert_eq!(parsed.evolved_norms[0].name, "test_norm");
    assert_eq!(parsed.evolved_norms[0].action, NormAction::Prefer);
}
