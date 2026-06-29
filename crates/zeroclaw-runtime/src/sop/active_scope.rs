use std::sync::{Arc, Mutex};

use super::types::SopStep;
use zeroclaw_config::schema::SopConfig;

/// The active SOP step's additional tool exclusions for the agent turn loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveStepScope {
    pub run_id: String,
    pub step_number: u32,
    pub excluded: Vec<String>,
}

pub type ActiveScopeHandle = Arc<Mutex<Option<ActiveStepScope>>>;

/// Resolve the active step's enforced tool scope, if step-scope enforcement is
/// enabled and the step declares a scope.
pub fn resolve_active_step_scope(
    run_id: &str,
    step: &SopStep,
    config: &SopConfig,
    registry_names: &[String],
) -> Option<ActiveStepScope> {
    if !config.step_scope_enforce {
        return None;
    }
    let scope = step.effective_tool_scope()?;
    let excluded =
        super::scope::resolve_excluded(registry_names, &scope, None, &config.step_mandatory_tools);
    Some(ActiveStepScope {
        run_id: run_id.to_string(),
        step_number: step.number,
        excluded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::{SopStep, StepToolScope};

    #[test]
    fn active_step_scope_only_resolves_when_enforcement_enabled() {
        let step = SopStep {
            number: 2,
            scope: Some(StepToolScope {
                allow: Some(vec!["read_file".into()]),
                deny: Vec::new(),
            }),
            ..SopStep::default()
        };
        let registry = vec!["read_file".to_string(), "shell".to_string()];

        assert!(
            resolve_active_step_scope("run-1", &step, &SopConfig::default(), &registry).is_none()
        );

        let config = SopConfig {
            step_scope_enforce: true,
            ..SopConfig::default()
        };
        let active = resolve_active_step_scope("run-1", &step, &config, &registry)
            .expect("enforced step scope should resolve");

        assert_eq!(active.run_id, "run-1");
        assert_eq!(active.step_number, 2);
        assert_eq!(active.excluded, vec!["shell".to_string()]);
    }

    #[test]
    fn active_step_scope_preserves_mandatory_tools() {
        let step = SopStep {
            number: 1,
            scope: Some(StepToolScope {
                allow: Some(Vec::new()),
                deny: vec!["sop_status".into()],
            }),
            ..SopStep::default()
        };
        let registry = vec!["read_file".to_string(), "sop_status".to_string()];
        let config = SopConfig {
            step_scope_enforce: true,
            step_mandatory_tools: vec!["sop_status".into()],
            ..SopConfig::default()
        };

        let active = resolve_active_step_scope("run-1", &step, &config, &registry)
            .expect("enforced step scope should resolve");

        assert_eq!(active.excluded, vec!["read_file".to_string()]);
    }
}
