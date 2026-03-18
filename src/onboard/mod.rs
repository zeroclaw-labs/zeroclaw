pub mod wizard;

// Re-exported for CLI and external use
#[allow(unused_imports)]
pub use wizard::{
    load_provider_defaults, refresh_models_quiet, resolve_default_model_for_provider,
    run_channels_repair_wizard, run_models_list, run_models_refresh, run_models_refresh_all,
    run_models_set, run_models_status, run_quick_setup, run_wizard, save_provider_default,
    MODEL_CACHE_TTL_SECS,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reexport_exists<F>(_value: F) {}

    #[test]
    fn wizard_functions_are_reexported() {
        assert_reexport_exists(run_channels_repair_wizard);
        assert_reexport_exists(run_quick_setup);
        assert_reexport_exists(run_wizard);
        assert_reexport_exists(run_models_refresh);
        assert_reexport_exists(run_models_list);
        assert_reexport_exists(run_models_set);
        assert_reexport_exists(run_models_status);
        assert_reexport_exists(run_models_refresh_all);
        assert_reexport_exists(refresh_models_quiet);
        assert_reexport_exists(load_provider_defaults);
        assert_reexport_exists(save_provider_default);
        assert_reexport_exists(resolve_default_model_for_provider);
        let _ = MODEL_CACHE_TTL_SECS;
    }
}
