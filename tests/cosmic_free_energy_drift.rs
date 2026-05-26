use zeroclaw::cosmic::{DriftDetector, FreeEnergyState};

#[test]
fn free_energy_rises_on_surprise() {
    let mut fe = FreeEnergyState::new(100);
    let pid = fe.predict("safety", 0.9, 0.8);
    fe.observe(&pid, 0.1);

    assert!(fe.free_energy() > 0.0);
    assert!(fe.should_update_model("safety", 0.3));
}

#[test]
fn drift_detector_flags_diverging_subsystem() {
    let mut detector = DriftDetector::new(5, 0.3);
    for i in 0..5 {
        detector.record_sample("policy", 0.5 + (i as f64) * 0.2);
    }

    assert!(detector.is_drifting("policy"));
    let report = detector.drift_report();
    assert!(report.drifting_count > 0);
}

#[test]
fn free_energy_and_drift_cross_signal() {
    let mut fe = FreeEnergyState::new(100);
    let mut detector = DriftDetector::new(10, 0.5);

    for i in 0..6 {
        let predicted = 0.5;
        let actual = 0.5 + (i as f64) * 0.15;
        let pid = fe.predict("tool_use", predicted, 0.7);
        if let Some(err) = fe.observe(&pid, actual) {
            detector.record_sample("tool_use", err.error_magnitude);
        }
    }

    let energy = fe.free_energy();
    assert!(
        energy > 0.0,
        "free energy should be elevated after surprises"
    );

    let report = detector.drift_report();
    assert_eq!(report.total_subsystems, 1);
}

#[test]
fn accurate_predictions_keep_energy_low() {
    let mut fe = FreeEnergyState::new(100);
    for _ in 0..5 {
        let pid = fe.predict("stable_domain", 1.0, 0.9);
        fe.observe(&pid, 1.0);
    }

    assert!(
        fe.free_energy() < 0.1,
        "accurate predictions should keep free energy near zero"
    );
    assert!(!fe.should_update_model("stable_domain", 0.3));
}

#[test]
fn drift_detector_stable_subsystem_no_alert() {
    let mut detector = DriftDetector::new(10, 0.5);
    for _ in 0..10 {
        detector.record_sample("stable", 0.5);
    }

    assert!(!detector.is_drifting("stable"));
    let report = detector.drift_report();
    assert_eq!(report.drifting_count, 0);
}
