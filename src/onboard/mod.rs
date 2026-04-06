pub mod wizard;

// Re-exported for CLI and external use
#[allow(unused_imports)]
pub use wizard::{
    CachedModels, ProjectContext, backend_key_from_choice, cache_live_models_for_provider,
    curated_models_for_provider, fetch_live_models_for_provider, load_cached_models_for_provider,
    memory_config_defaults_for_backend, run_channels_repair_wizard, run_models_list,
    run_models_refresh, run_models_refresh_all, run_models_set, run_models_status, run_quick_setup,
    run_wizard, scaffold_workspace, supports_live_model_fetch,
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
        assert_reexport_exists(backend_key_from_choice);
        assert_reexport_exists(memory_config_defaults_for_backend);
        assert_reexport_exists(scaffold_workspace);
        assert_reexport_exists(curated_models_for_provider);
        assert_reexport_exists(supports_live_model_fetch);
    }
}
