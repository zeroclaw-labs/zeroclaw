pub mod tui;
pub mod web;
pub mod wizard;

// Re-exported for CLI and external use
#[allow(unused_imports)]
pub use tui::{run_wizard_tui, run_wizard_tui_with_migration};
pub use web::run_onboard_web;
#[allow(unused_imports)]
pub use wizard::{
    curated_models_for_provider, default_model_for_provider, memory_config_defaults_for_backend,
    run_channels_repair_wizard, run_models_list, run_models_refresh, run_models_refresh_all,
    run_models_set, run_models_status, run_quick_setup, run_quick_setup_with_migration, run_wizard,
    run_wizard_with_migration, scaffold_workspace, OpenClawOnboardMigrationOptions,
    ProjectContext,
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
        assert_reexport_exists(run_quick_setup_with_migration);
        assert_reexport_exists(run_wizard_with_migration);
        assert_reexport_exists(run_wizard_tui);
        assert_reexport_exists(run_wizard_tui_with_migration);
        let _: Option<OpenClawOnboardMigrationOptions> = None;
        assert_reexport_exists(run_models_refresh);
        assert_reexport_exists(run_models_list);
        assert_reexport_exists(run_models_set);
        assert_reexport_exists(run_models_status);
        assert_reexport_exists(run_models_refresh_all);
    }
}
