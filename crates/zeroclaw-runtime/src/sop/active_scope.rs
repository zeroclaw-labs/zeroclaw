use std::future::Future;
use std::sync::{Arc, Mutex};

use super::types::SopStep;
use zeroclaw_config::schema::SopConfig;

/// The SOP control surface. A step body must never drive its own run: allowing
/// these inside a step turn lets the model advance, execute, or approve the very
/// procedure it is a step of. Excluded on every SOP step turn — live and
/// headless — regardless of whether the step declares a scope.
pub const SOP_CONTROL_TOOLS: [&str; 3] = ["sop_execute", "sop_advance", "sop_approve"];

/// The active SOP step's additional tool exclusions for the agent turn loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveStepScope {
    pub run_id: String,
    pub step_number: u32,
    pub excluded: Vec<String>,
}

pub type ActiveScopeHandle = Arc<Mutex<Option<ActiveStepScope>>>;

/// A headless step's tool-scope contract, carried into the fresh `agent::run`
/// that executes it.
///
/// The live nested-step driver narrows a step's tool surface inside the
/// enclosing turn (`sop_step_excluded_tools`), where the assembled registry is
/// already in hand. A headless step has no enclosing turn: `agent::run` builds
/// the registry itself, so the scope cannot be resolved at the call site. This
/// carries the contract into that loop, which resolves it against the real tool
/// names once they exist — and re-resolves per turn, so tools activated
/// mid-run (`tool_search`) are narrowed too.
#[derive(Debug, Clone)]
pub struct HeadlessStepScope {
    pub run_id: String,
    pub step: SopStep,
    pub config: SopConfig,
}

impl HeadlessStepScope {
    /// Tools this step may not call, resolved against the callable registry.
    ///
    /// Always excludes [`SOP_CONTROL_TOOLS`]; adds the step's declared scope
    /// when `sop.step_scope_enforce` is on. Mirrors the live path's
    /// `sop_step_excluded_tools` so both routes enforce one contract.
    pub fn excluded(&self, registry_names: &[String]) -> Vec<String> {
        let mut excluded: Vec<String> =
            SOP_CONTROL_TOOLS.iter().map(|t| (*t).to_string()).collect();
        if let Some(active) =
            resolve_active_step_scope(&self.run_id, &self.step, &self.config, registry_names)
        {
            for tool in active.excluded {
                if !excluded.iter().any(|e| e.eq_ignore_ascii_case(&tool)) {
                    excluded.push(tool);
                }
            }
        }
        excluded.sort();
        excluded
    }
}

tokio::task_local! {
    /// The headless step scope in force on this task.
    ///
    /// Set once around the step's `agent::run` by the headless driver, which is
    /// the single site that resolves the scope. Tool calls execute inline on
    /// that task (the turn loop awaits them directly), so a tool that starts a
    /// nested run can read the boundary it is running under instead of being
    /// handed a copy through every registry constructor between the two.
    static ACTIVE_HEADLESS_STEP_SCOPE: HeadlessStepScope;
}

/// Run `future` with `scope` as the active headless step scope.
pub async fn with_active_headless_step_scope<T>(
    scope: HeadlessStepScope,
    future: impl Future<Output = T>,
) -> T {
    ACTIVE_HEADLESS_STEP_SCOPE.scope(scope, future).await
}

/// Re-establish an inherited scope on a task that could not inherit it.
///
/// A task-local does not cross `spawn`, so a caller that hands step work to a
/// background task captures [`active_headless_step_scope`] first and restores it
/// inside. `None` runs `future` unchanged.
pub async fn with_inherited_headless_step_scope<T>(
    scope: Option<HeadlessStepScope>,
    future: impl Future<Output = T>,
) -> T {
    match scope {
        Some(scope) => with_active_headless_step_scope(scope, future).await,
        None => future.await,
    }
}

/// The headless step scope this task is running under, if any.
///
/// A tool that starts a child agent run passes this into the child's
/// `AgentRunOverrides`: spawning a child does not widen the step's capability
/// boundary, so the child re-resolves the same exclusions against its own
/// registry. `None` outside a headless step, where there is no boundary to
/// carry.
#[must_use]
pub fn active_headless_step_scope() -> Option<HeadlessStepScope> {
    ACTIVE_HEADLESS_STEP_SCOPE.try_with(Clone::clone).ok()
}

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

    fn headless_scope(step: SopStep, config: SopConfig) -> HeadlessStepScope {
        HeadlessStepScope {
            run_id: "run-1".into(),
            step,
            config,
        }
    }

    /// A headless step gets the SOP control surface removed even when it
    /// declares no scope and enforcement is off. Otherwise an unattended step
    /// could advance, execute, or approve its own run.
    #[test]
    fn headless_scope_always_removes_the_sop_control_surface() {
        let registry = vec![
            "read_file".to_string(),
            "shell".to_string(),
            "sop_advance".to_string(),
            "sop_execute".to_string(),
            "sop_approve".to_string(),
        ];
        let scope = headless_scope(SopStep::default(), SopConfig::default());

        let excluded = scope.excluded(&registry);

        for tool in SOP_CONTROL_TOOLS {
            assert!(
                excluded.iter().any(|e| e == tool),
                "{tool} must be excluded from a headless step, got {excluded:?}"
            );
        }
        assert!(
            !excluded.iter().any(|e| e == "read_file"),
            "an unscoped step keeps the rest of its registry, got {excluded:?}"
        );
    }

    /// With enforcement on, the step's declared scope narrows the surface the
    /// same way the live nested-step driver does.
    #[test]
    fn headless_scope_applies_the_declared_step_scope() {
        let registry = vec![
            "read_file".to_string(),
            "shell".to_string(),
            "write_file".to_string(),
        ];
        let scope = headless_scope(
            SopStep {
                number: 3,
                scope: Some(StepToolScope {
                    allow: Some(vec!["read_file".into()]),
                    deny: Vec::new(),
                }),
                ..SopStep::default()
            },
            SopConfig {
                step_scope_enforce: true,
                ..SopConfig::default()
            },
        );

        let excluded = scope.excluded(&registry);

        assert!(excluded.iter().any(|e| e == "shell"));
        assert!(excluded.iter().any(|e| e == "write_file"));
        assert!(
            !excluded.iter().any(|e| e == "read_file"),
            "the allowed tool must survive, got {excluded:?}"
        );
    }

    /// A step cannot re-open the SOP control surface by naming it in `allow`
    /// or by riding on `step_mandatory_tools`: the control tools are seeded
    /// into the exclusion set before the declared scope is consulted.
    #[test]
    fn headless_scope_control_surface_survives_a_permissive_step() {
        let registry = vec!["read_file".to_string(), "sop_advance".to_string()];
        let scope = headless_scope(
            SopStep {
                number: 1,
                scope: Some(StepToolScope {
                    allow: Some(vec!["read_file".into(), "sop_advance".into()]),
                    deny: Vec::new(),
                }),
                ..SopStep::default()
            },
            SopConfig {
                step_scope_enforce: true,
                step_mandatory_tools: vec!["sop_advance".into()],
                ..SopConfig::default()
            },
        );

        let excluded = scope.excluded(&registry);

        assert!(
            excluded.iter().any(|e| e == "sop_advance"),
            "a step must not be able to allow itself the SOP control surface, got {excluded:?}"
        );
    }
}
