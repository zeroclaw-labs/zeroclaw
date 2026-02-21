pub mod feature_packs;
pub mod wizard;

#[allow(unused_imports)]
pub use feature_packs::{
    feature_pack_by_id, preset_by_id, FeaturePack, Preset, FEATURE_PACKS, PRESETS,
};
pub use wizard::{
    autonomy_config_for_security_profile_id, recommend_security_profile,
    run_channels_repair_wizard, run_models_refresh, run_quick_setup, run_wizard,
    security_profile_id_from_autonomy, security_profile_label, SecurityProfileRecommendation,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reexport_exists<F>(_value: F) {}

    #[test]
    fn wizard_functions_are_reexported() {
        assert_reexport_exists(run_wizard);
        assert_reexport_exists(run_channels_repair_wizard);
        assert_reexport_exists(run_quick_setup);
        assert_reexport_exists(run_models_refresh);
        assert_reexport_exists(feature_pack_by_id);
        assert_reexport_exists(preset_by_id);
    }
}
