use zeroclaw::cosmic::{EmotionalModulator, FreeEnergyState, GlobalVariable};

#[test]
fn modulator_responds_to_free_energy_signal() {
    let mut modulator = EmotionalModulator::new();
    let mut fe = FreeEnergyState::new(100);

    let pid = fe.predict("tool_reliability", 0.9, 0.8);
    fe.observe(&pid, 0.2);

    let energy = fe.free_energy();
    let surprise = fe.domain_surprise("tool_reliability").unwrap_or(0.0);
    modulator.apply_free_energy_signal(energy, surprise);

    let bias = modulator.compute_bias();
    assert!(
        bias.speed_vs_caution < 0.5,
        "high surprise should shift toward caution"
    );
}

#[test]
fn cognitive_load_affects_exploration() {
    let mut modulator = EmotionalModulator::new();
    modulator.apply_cognitive_load(20, 0.9);

    assert!(modulator.is_overloaded());
    assert!(!modulator.should_explore());
}

#[test]
fn low_load_encourages_exploration() {
    let mut modulator = EmotionalModulator::new();
    modulator.apply_cognitive_load(2, 0.1);

    assert!(!modulator.is_overloaded());
}

#[test]
fn emotional_input_shifts_valence() {
    let mut modulator = EmotionalModulator::new();
    let before = modulator.get_variable(GlobalVariable::Valence);
    modulator.apply_emotional_input(0.8, 0.5, 0.7);
    let after = modulator.get_variable(GlobalVariable::Valence);

    assert!(
        (after - before).abs() > 0.01,
        "emotional input should shift valence"
    );
}

#[test]
fn snapshot_captures_current_state() {
    let mut modulator = EmotionalModulator::new();
    modulator.set_variable(GlobalVariable::Urgency, 0.9);
    modulator.set_variable(GlobalVariable::Confidence, 0.3);

    let snap = modulator.snapshot();
    assert!(snap.variables.len() >= 2);
    let bias = snap.behavioral_bias;
    assert!(bias.speed_vs_caution >= 0.0);
    assert!(bias.speed_vs_caution <= 1.0);
}
