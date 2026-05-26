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
        arousal: None,
        confidence: None,
        risk_level: None,
        free_energy: None,
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
        arousal: None,
        confidence: None,
        risk_level: None,
        free_energy: None,
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

#[test]
fn cosmic_none_signals_preserve_original_behavior() {
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
    let state_none = clean_self_state();
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &state_none);
    assert_eq!(verdict, GateVerdict::Allow);

    let (tool_verdict, _) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &state_none,
        &[],
        None,
        None,
    );
    assert_eq!(tool_verdict, GateVerdict::Ask);
}

#[test]
fn high_arousal_makes_gate_more_cautious() {
    let base_state = clean_self_state();
    let aroused_state = SelfState {
        arousal: Some(0.95),
        ..clean_self_state()
    };

    let (_, base_score) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &base_state,
        &[],
        None,
        None,
    );
    let (_, aroused_score) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &aroused_state,
        &[],
        None,
        None,
    );
    assert!(
        aroused_score < base_score,
        "High arousal should reduce score: base={base_score} aroused={aroused_score}"
    );
}

#[test]
fn low_confidence_shifts_allow_to_ask() {
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

    let low_conf_state = SelfState {
        confidence: Some(0.1),
        ..clean_self_state()
    };
    let verdict = conscience_gate(&ctx, &Thresholds::default(), &low_conf_state);
    assert_eq!(verdict, GateVerdict::Ask);
}

#[test]
fn low_confidence_shifts_tool_allow_to_ask() {
    let normal_state = clean_self_state();
    let (normal_verdict, _) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &normal_state,
        &[],
        Some(0.99),
        None,
    );
    assert_eq!(normal_verdict, GateVerdict::Allow);

    let low_conf_state = SelfState {
        confidence: Some(0.1),
        ..clean_self_state()
    };
    let (verdict, _) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &low_conf_state,
        &[],
        Some(0.99),
        None,
    );
    assert_eq!(verdict, GateVerdict::Ask);
}

#[test]
fn high_free_energy_adds_score_penalty() {
    let base_state = clean_self_state();
    let high_fe_state = SelfState {
        free_energy: Some(0.95),
        ..clean_self_state()
    };

    let (_, base_score) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &base_state,
        &[],
        None,
        None,
    );
    let (_, fe_score) = evaluate_tool_call(
        "lookup",
        &Thresholds::default(),
        &high_fe_state,
        &[],
        None,
        None,
    );
    assert!(
        fe_score < base_score,
        "High free energy should reduce score: base={base_score} fe={fe_score}"
    );
}

#[test]
fn cosmic_bridge_extracts_signals() {
    use crate::cosmic::{EmotionalModulator, FreeEnergyState, GlobalVariable};

    let mut modulator = EmotionalModulator::new();
    modulator.set_variable(GlobalVariable::Arousal, 0.85);
    modulator.set_variable(GlobalVariable::Confidence, 0.2);
    modulator.set_variable(GlobalVariable::Risk, 0.6);

    let mut fe = FreeEnergyState::new(100);
    let id = fe.predict("test", 0.5, 0.9);
    fe.observe(&id, 0.99);

    let base = clean_self_state();
    let enriched = super::cosmic_bridge::self_state_from_cosmic(&modulator, &fe, &base);

    assert!((enriched.arousal.unwrap() - 0.85).abs() < f64::EPSILON);
    assert!((enriched.confidence.unwrap() - 0.2).abs() < f64::EPSILON);
    assert!((enriched.risk_level.unwrap() - 0.6).abs() < f64::EPSILON);
    assert!(enriched.free_energy.unwrap() > 0.0);
    assert_eq!(enriched.integrity_score, base.integrity_score);
    assert_eq!(enriched.recent_violations, base.recent_violations);
}
