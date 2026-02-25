use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use zeroclaw::cosmic::{
    Constitution, CosmicGate, CosmicMemoryGraph, CosmicPersistence, CosmicSnapshot,
    CounterfactualEngine, DriftDetector, EmotionalModulator, FreeEnergyState, GlobalVariable,
    GlobalWorkspace, InputSignal, IntegrationMeter, NormKind, NormativeEngine, PolicyEngine,
    Scenario, SensoryThalamus, SignalSource, SubsystemId,
};

fn make_signal(source: SignalSource, content: &str, raw_salience: f64) -> InputSignal {
    InputSignal {
        source,
        content: content.to_string(),
        raw_salience,
        timestamp: Utc::now(),
    }
}

#[test]
fn full_pipeline_signal_to_action() {
    let mut thalamus = SensoryThalamus::new(0.2, 100);
    let mut workspace = GlobalWorkspace::new(0.2, 5, 100);

    workspace.register_subsystem(SubsystemId::Memory, 0.9);
    workspace.register_subsystem(SubsystemId::FreeEnergy, 0.8);
    workspace.register_subsystem(SubsystemId::Causality, 0.7);
    workspace.register_subsystem(SubsystemId::SelfModel, 0.6);
    workspace.register_subsystem(SubsystemId::WorldModel, 0.5);
    workspace.register_subsystem(SubsystemId::Normative, 0.8);
    workspace.register_subsystem(SubsystemId::Modulation, 0.4);
    workspace.register_subsystem(SubsystemId::Policy, 0.7);
    workspace.register_subsystem(SubsystemId::Counterfactual, 0.5);
    workspace.register_subsystem(SubsystemId::Consolidation, 0.3);
    workspace.register_subsystem(SubsystemId::Drift, 0.4);
    workspace.register_subsystem(SubsystemId::Constitution, 0.9);

    let signal = make_signal(
        SignalSource::Channel,
        "user request: deploy application",
        0.9,
    );
    let salience = thalamus.process_signal(&signal);
    assert!(salience.is_some());
    let scored = salience.unwrap();
    assert!(scored.score >= 0.2);

    workspace.activate(SubsystemId::Memory, 0.8);
    workspace.activate(SubsystemId::Normative, 0.9);
    workspace.activate(SubsystemId::Policy, 0.7);
    workspace.activate(SubsystemId::Constitution, 0.85);
    workspace.activate(SubsystemId::FreeEnergy, 0.6);

    let broadcast = workspace.compete();
    assert!(!broadcast.active_subsystems.is_empty());
    assert!(broadcast.dominant.is_some());
    assert!(broadcast.coherence > 0.0);

    let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let gate = CosmicGate::new(normative, policy, counterfactual);

    let decision = gate.check_action("deploy", "deploy application to production");
    assert!(decision.allowed);

    let snap = thalamus.snapshot();
    assert_eq!(snap.signals_processed, 1);
    assert_eq!(snap.signals_passed, 1);
}

#[test]
fn gate_blocks_harmful_action() {
    let mut ne = NormativeEngine::new(100, 100);
    ne.register_norm(
        "no_delete",
        NormKind::Prohibition,
        "safety",
        "never delete production data or destroy systems",
        1.0,
    );

    let normative = Arc::new(Mutex::new(ne));
    let policy = Arc::new(Mutex::new(PolicyEngine::new(100)));
    let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 100)));
    let gate = CosmicGate::new(normative, policy, counterfactual);

    let blocked = gate.check_action("shell", "delete production data");
    assert!(!blocked.allowed);
    assert!(blocked.reason.is_some());
    assert!((blocked.risk_score - 1.0).abs() < f64::EPSILON);

    let allowed = gate.check_action("shell", "read configuration file");
    assert!(allowed.allowed);
}

#[test]
fn drift_triggers_feedback_loop() {
    let mut drift = DriftDetector::new(20, 0.1);
    let mut modulator = EmotionalModulator::new();
    let mut thalamus = SensoryThalamus::new(0.3, 100);

    for _ in 0..10 {
        drift.record_sample("latency", 0.2);
    }
    for _ in 0..10 {
        drift.record_sample("latency", 0.8);
    }

    let report = drift.drift_report();
    assert!(report.drifting_count > 0);
    assert!(report.max_drift > 0.1);

    let urgency_before = modulator.get_variable(GlobalVariable::Urgency);
    modulator.nudge_variable(GlobalVariable::Urgency, report.max_drift * 0.5);
    let urgency_after = modulator.get_variable(GlobalVariable::Urgency);
    assert!(urgency_after > urgency_before);

    let threshold_before = 0.3_f64;
    let arousal = modulator.get_variable(GlobalVariable::Arousal);
    thalamus.adjust_threshold(arousal);
    let snap = thalamus.snapshot();
    assert!((snap.attention_threshold - threshold_before).abs() > f64::EPSILON);
}

#[test]
fn behavioral_bias_influences_temperature() {
    let mut modulator = EmotionalModulator::new();

    modulator.set_variable(GlobalVariable::Urgency, 0.9);
    modulator.set_variable(GlobalVariable::Arousal, 0.8);
    let high_bias = modulator.compute_bias();
    assert!(high_bias.speed_vs_caution > 0.5);
    let high_temp_adjust = (high_bias.speed_vs_caution - 0.5) * 0.2;
    assert!(high_temp_adjust > 0.0);

    modulator.set_variable(GlobalVariable::Urgency, 0.1);
    modulator.set_variable(GlobalVariable::Arousal, 0.1);
    modulator.set_variable(GlobalVariable::Risk, 0.9);
    let low_bias = modulator.compute_bias();
    let low_temp_adjust = (low_bias.speed_vs_caution - 0.5) * 0.2;
    assert!(low_temp_adjust < 0.0);
}

#[test]
fn workspace_competition_selects_top_n() {
    let mut ws = GlobalWorkspace::new(0.1, 3, 100);

    ws.register_subsystem(SubsystemId::Memory, 0.9);
    ws.register_subsystem(SubsystemId::FreeEnergy, 0.8);
    ws.register_subsystem(SubsystemId::Causality, 0.7);
    ws.register_subsystem(SubsystemId::SelfModel, 0.6);
    ws.register_subsystem(SubsystemId::WorldModel, 0.5);
    ws.register_subsystem(SubsystemId::Normative, 0.4);

    ws.activate(SubsystemId::Memory, 0.95);
    ws.activate(SubsystemId::FreeEnergy, 0.85);
    ws.activate(SubsystemId::Causality, 0.75);
    ws.activate(SubsystemId::SelfModel, 0.65);
    ws.activate(SubsystemId::WorldModel, 0.55);
    ws.activate(SubsystemId::Normative, 0.45);

    let result = ws.compete();
    assert_eq!(result.active_subsystems.len(), 3);
    assert_eq!(result.dominant, Some(SubsystemId::Memory));
    assert!(result.suppressed_subsystems.len() >= 3);
}

#[test]
fn thalamus_filters_noise_passes_novel() {
    let mut thalamus = SensoryThalamus::new(0.3, 100);

    let mut saliences = Vec::new();
    for _i in 0..5 {
        let sig = make_signal(SignalSource::Channel, "repeated boring message", 0.5);
        if let Some(scored) = thalamus.process_signal(&sig) {
            saliences.push(scored.score);
        }
    }

    let first_novelty = thalamus.novelty_score("repeated boring message");
    assert!(first_novelty < 0.5);

    let novel = make_signal(
        SignalSource::Channel,
        "completely unique alert never seen",
        0.8,
    );
    let novel_result = thalamus.process_signal(&novel);
    assert!(novel_result.is_some());
    let novel_scored = novel_result.unwrap();

    if let Some(last_repeated) = saliences.last() {
        assert!(
            novel_scored.score > *last_repeated,
            "novel ({}) should score higher than habituated ({})",
            novel_scored.score,
            last_repeated,
        );
    }
}

#[test]
fn persistence_round_trip_full_state() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let persistence = CosmicPersistence::new(dir.path());

    let modulator = EmotionalModulator::new();
    let mod_snap = modulator.snapshot();

    let mut drift = DriftDetector::new(10, 0.1);
    drift.record_sample("cpu", 0.5);
    drift.record_sample("cpu", 0.6);
    let drift_report = drift.drift_report();

    let mut thalamus = SensoryThalamus::new(0.3, 100);
    let sig = make_signal(SignalSource::Channel, "test signal", 0.9);
    thalamus.process_signal(&sig);
    let thalamus_snap = thalamus.snapshot();

    let mut modules = HashMap::new();
    modules.insert(
        "modulation".to_string(),
        serde_json::to_value(&mod_snap).unwrap(),
    );
    modules.insert(
        "drift".to_string(),
        serde_json::to_value(&drift_report).unwrap(),
    );
    modules.insert(
        "thalamus".to_string(),
        serde_json::to_value(&thalamus_snap).unwrap(),
    );

    let snapshot = CosmicSnapshot {
        modules,
        version: 1,
        saved_at: Utc::now(),
    };

    persistence.save_all(&snapshot).unwrap();
    let loaded = persistence.load_all().unwrap();

    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.modules.len(), 3);
    assert!(loaded.modules.contains_key("modulation"));
    assert!(loaded.modules.contains_key("drift"));
    assert!(loaded.modules.contains_key("thalamus"));

    let loaded_thalamus: zeroclaw::cosmic::ThalamusSnapshot =
        serde_json::from_value(loaded.modules["thalamus"].clone()).unwrap();
    assert_eq!(loaded_thalamus.signals_processed, 1);
    assert_eq!(loaded_thalamus.signals_passed, 1);
}

#[test]
fn constitution_integrity_survives_pipeline() {
    let mut constitution = Constitution::new();
    constitution.register_value("safety", "protect users from harm", 1.0, true);
    constitution.register_value("honesty", "always provide truthful information", 0.9, true);
    constitution.register_value("helpfulness", "assist users effectively", 0.8, false);
    constitution.seal();

    let check1 = constitution.verify_integrity();
    assert!(check1.passed);

    let alignment = constitution.check_action_alignment("help users achieve goals");
    assert!(alignment >= 0.0);

    let alignment2 = constitution.check_action_alignment("provide truthful information to users");
    assert!(alignment2 >= 0.0);

    let check2 = constitution.verify_integrity();
    assert!(check2.passed);
    assert_eq!(check1.expected_hash, check2.expected_hash);

    assert!(!constitution.remove_value("safety"));
    let check3 = constitution.verify_integrity();
    assert!(check3.passed);

    assert_eq!(constitution.immutable_count(), 2);
    assert_eq!(constitution.value_count(), 3);
}

#[test]
fn free_energy_prediction_correction_cycle() {
    let mut fe = FreeEnergyState::new(100);

    let id1 = fe.predict("tool_success", 0.5, 0.8);
    let error1 = fe.observe(&id1, 0.9).unwrap();
    let fe_after_bad = fe.free_energy();
    assert!(fe_after_bad > 0.0);
    assert!(error1.surprise > 0.0);

    let id2 = fe.predict("tool_success", 0.88, 0.9);
    let error2 = fe.observe(&id2, 0.9).unwrap();
    let fe_after_better = fe.free_energy();

    assert!(
        fe_after_better < fe_after_bad,
        "free energy should decrease with better prediction: {} < {}",
        fe_after_better,
        fe_after_bad,
    );
    assert!(error2.error_magnitude.abs() < error1.error_magnitude.abs());

    let acc = fe.accuracy("tool_success");
    assert!(acc.is_some());
}

#[test]
fn integration_meter_phi_increases_with_coupling() {
    let mut meter = IntegrationMeter::new();
    meter.register_subsystem("a", vec!["b".into()]);
    meter.register_subsystem("b", vec!["a".into()]);
    meter.update_state("a", 0.5);
    meter.update_state("b", 0.8);
    let phi_sparse = meter.compute_phi();
    assert!(phi_sparse > 0.0);

    let mut dense_meter = IntegrationMeter::new();
    dense_meter.register_subsystem("a", vec!["b".into(), "c".into(), "d".into()]);
    dense_meter.register_subsystem("b", vec!["a".into(), "c".into(), "d".into()]);
    dense_meter.register_subsystem("c", vec!["a".into(), "b".into(), "d".into()]);
    dense_meter.register_subsystem("d", vec!["a".into(), "b".into(), "c".into()]);
    dense_meter.update_state("a", 0.5);
    dense_meter.update_state("b", 0.5);
    dense_meter.update_state("c", 0.5);
    dense_meter.update_state("d", 0.5);
    let phi_dense = dense_meter.compute_phi();

    assert!(
        phi_dense > phi_sparse,
        "dense identical states phi ({}) should exceed sparse divergent phi ({})",
        phi_dense,
        phi_sparse,
    );

    let snap = dense_meter.snapshot();
    assert_eq!(snap.subsystem_count, 4);
    assert!(snap.edge_count > 0);
    assert!((snap.clustering_coefficient - 1.0).abs() < 1e-10);
}

#[test]
fn counterfactual_simulation_informs_gate() {
    let mut cf = CounterfactualEngine::new(10, 100);
    cf.update_world_state("system_load", 0.3);
    cf.update_world_state("user_trust", 0.8);

    let safe_scenario = Scenario {
        id: "safe_deploy".to_string(),
        action: "deploy with gradual rollout".to_string(),
        context: [("system_load", 0.3), ("user_trust", 0.8)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect(),
        created_at: Utc::now(),
    };

    let risky_scenario = Scenario {
        id: "risky_deploy".to_string(),
        action: "deploy all at once".to_string(),
        context: [("system_load", 0.9), ("user_trust", 0.2)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect(),
        created_at: Utc::now(),
    };

    let results = cf.compare_scenarios(&[safe_scenario, risky_scenario]);
    assert_eq!(results.len(), 2);
    assert!(results[0].predicted_outcome >= results[1].predicted_outcome);

    let best = results.first().unwrap();
    assert_eq!(best.scenario_id, "safe_deploy");
    assert!(best.risk < results[1].risk);
}

#[test]
fn memory_graph_spreading_activation_pipeline() {
    let mut graph = CosmicMemoryGraph::new(20);
    graph.insert_node(
        "deploy_cmd".into(),
        "deploy application".into(),
        "commands".into(),
        vec![0.9, 0.1],
    );
    graph.insert_node(
        "rollback_cmd".into(),
        "rollback deployment".into(),
        "commands".into(),
        vec![0.8, 0.2],
    );
    graph.insert_node(
        "deploy_hist".into(),
        "previous deployment succeeded".into(),
        "history".into(),
        vec![0.7, 0.3],
    );
    graph.insert_node(
        "error_hist".into(),
        "deployment error occurred".into(),
        "history".into(),
        vec![0.2, 0.8],
    );

    graph.insert_edge(
        "deploy_cmd".into(),
        "deploy_hist".into(),
        0.9,
        "caused".into(),
    );
    graph.insert_edge(
        "deploy_cmd".into(),
        "rollback_cmd".into(),
        0.7,
        "related".into(),
    );
    graph.insert_edge("deploy_cmd".into(), "error_hist".into(), 0.3, "risk".into());
    graph.insert_edge(
        "rollback_cmd".into(),
        "error_hist".into(),
        0.8,
        "triggered_by".into(),
    );

    zeroclaw::cosmic::spreading_activation(&mut graph, &["deploy_cmd".into()], 1.0, 0.5, 3);

    let deploy = graph.get_node("deploy_cmd").unwrap();
    let rollback = graph.get_node("rollback_cmd").unwrap();
    let history = graph.get_node("deploy_hist").unwrap();
    let error = graph.get_node("error_hist").unwrap();

    assert!(deploy.activation >= 1.0);
    assert!(history.activation > 0.0);
    assert!(rollback.activation > 0.0);
    assert!(error.activation > 0.0);
    assert!(history.activation > error.activation);

    let path = graph.strongest_path("deploy_cmd", "error_hist");
    assert!(path.is_some());
}

#[test]
fn end_to_end_modulation_drift_workspace_cycle() {
    let modulator = Arc::new(Mutex::new(EmotionalModulator::new()));
    let drift = Arc::new(Mutex::new(DriftDetector::new(20, 0.1)));
    let thalamus = Arc::new(Mutex::new(SensoryThalamus::new(0.3, 100)));
    let workspace = Arc::new(Mutex::new(GlobalWorkspace::new(0.2, 5, 100)));

    {
        let mut ws = workspace.lock();
        ws.register_subsystem(SubsystemId::Memory, 0.9);
        ws.register_subsystem(SubsystemId::FreeEnergy, 0.8);
        ws.register_subsystem(SubsystemId::Normative, 0.7);
        ws.register_subsystem(SubsystemId::Modulation, 0.6);
        ws.register_subsystem(SubsystemId::Drift, 0.5);
    }

    for i in 0..5 {
        let signal = make_signal(
            SignalSource::Channel,
            &format!("user message {i}"),
            0.7 + (i as f64 * 0.05),
        );

        let salience = {
            let mut t = thalamus.lock();
            t.process_signal(&signal)
        };

        if let Some(scored) = salience {
            let mut ws = workspace.lock();
            ws.activate(SubsystemId::Memory, scored.score);
            ws.activate(SubsystemId::FreeEnergy, scored.urgency);
        }

        {
            let mut d = drift.lock();
            d.record_sample("signal_salience", 0.5 + (i as f64 * 0.1));
        }
    }

    let report = {
        let d = drift.lock();
        d.drift_report()
    };

    if report.drifting_count > 0 {
        let mut m = modulator.lock();
        m.nudge_variable(GlobalVariable::Urgency, 0.3);
        m.nudge_variable(GlobalVariable::Arousal, 0.2);
    }

    let bias = {
        let m = modulator.lock();
        m.compute_bias()
    };

    {
        let mut t = thalamus.lock();
        let arousal = {
            let m = modulator.lock();
            m.get_variable(GlobalVariable::Arousal)
        };
        t.adjust_threshold(arousal);
    }

    let broadcast = {
        let mut ws = workspace.lock();
        ws.compete()
    };

    assert!(!broadcast.active_subsystems.is_empty());
    assert!(bias.speed_vs_caution >= 0.0 && bias.speed_vs_caution <= 1.0);

    let snap = {
        let t = thalamus.lock();
        t.snapshot()
    };
    assert_eq!(snap.signals_processed, 5);
}
