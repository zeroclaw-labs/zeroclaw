use super::types::SelfState;
use crate::cosmic::{EmotionalModulator, FreeEnergyState, GlobalVariable};

pub fn self_state_from_cosmic(
    modulator: &EmotionalModulator,
    free_energy: &FreeEnergyState,
    base_state: &SelfState,
) -> SelfState {
    SelfState {
        integrity_score: base_state.integrity_score,
        recent_violations: base_state.recent_violations,
        active_repairs: base_state.active_repairs,
        arousal: Some(modulator.get_variable(GlobalVariable::Arousal)),
        confidence: Some(modulator.get_variable(GlobalVariable::Confidence)),
        risk_level: Some(modulator.get_variable(GlobalVariable::Risk)),
        free_energy: Some(free_energy.free_energy()),
    }
}
