//! Binary-local conscience tests. The pure gate/ledger/types tests now
//! live in the `zeroclaw-conscience` crate; only the `cosmic_bridge`
//! adapter test stays here because it depends on `crate::cosmic`.
use super::*;

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
