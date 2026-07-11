pub mod failure;
pub mod guard;

use super::condition::evaluate_condition;
use super::rundata::RunData;
use super::types::{Sop, SopRun, SopStep, SopStepStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    Step(u32),
    Retry,
    Complete,
    Fail(String),
    Wait(u32),
}

pub struct RouteCtx<'a> {
    pub sop: &'a Sop,
    pub run: &'a SopRun,
    pub run_data: &'a RunData,
    pub last_status: SopStepStatus,
    pub max_step_visits: u32,
}

/// Pick the next step, preserving linear behavior when no routing is declared.
pub fn resolve_next(ctx: &RouteCtx<'_>) -> NextStep {
    if ctx.last_status == SopStepStatus::Failed {
        return NextStep::Fail("step failed".into());
    }

    let Some(current) = ctx
        .sop
        .steps
        .iter()
        .find(|step| step.number == ctx.run.current_step)
    else {
        return NextStep::Complete;
    };

    if let Some(when) = current.routing.when.as_deref()
        && !evaluate_condition(when, Some(&ctx.run_data.to_payload().to_string()))
    {
        return NextStep::Complete;
    }

    // ── Switch: evaluate ports top to bottom; first passing rule routes. A
    // rule with no `when` is the catch-all. No rule matches → Complete.
    if !current.routing.switch.is_empty() {
        let payload = ctx.run_data.to_payload().to_string();
        for rule in &current.routing.switch {
            let matched = match rule.when.as_deref() {
                Some(when) => evaluate_condition(when, Some(&payload)),
                None => true,
            };
            if !matched {
                continue;
            }
            let Some(target) = rule.goto else {
                return NextStep::Fail(format!("switch port '{}' has no target", rule.name));
            };
            let Some(step) = ctx.sop.steps.iter().find(|s| s.number == target) else {
                return NextStep::Fail(format!("step {target} does not exist"));
            };
            if !guard::within_visit_bound(ctx.run, target, ctx.max_step_visits) {
                return NextStep::Fail(format!("step {target} visit limit reached"));
            }
            return if eligible(step, ctx.run_data) {
                NextStep::Step(target)
            } else {
                NextStep::Wait(target)
            };
        }
        return NextStep::Complete;
    }

    let explicit_next = current.routing.next;
    if explicit_next.is_none() && current.routing.terminal {
        return NextStep::Complete;
    }
    let next_step = explicit_next.unwrap_or_else(|| ctx.run.current_step.saturating_add(1));
    let Some(step) = ctx.sop.steps.iter().find(|step| step.number == next_step) else {
        return if explicit_next.is_none() && next_step > ctx.run.total_steps {
            NextStep::Complete
        } else {
            NextStep::Fail(format!("step {next_step} does not exist"))
        };
    };
    if !guard::within_visit_bound(ctx.run, next_step, ctx.max_step_visits) {
        return NextStep::Fail(format!("step {next_step} visit limit reached"));
    }

    if eligible(step, ctx.run_data) {
        NextStep::Step(next_step)
    } else {
        NextStep::Wait(next_step)
    }
}

/// A step is eligible when all declared dependencies have produced outputs.
pub fn eligible(step: &SopStep, run_data: &RunData) -> bool {
    step.routing
        .depends_on
        .iter()
        .all(|dependency| run_data.outputs.contains_key(dependency))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{
        SopEvent, SopExecutionMode, SopPriority, SopRunStatus, SopTriggerSource,
    };

    fn step(number: u32) -> SopStep {
        SopStep {
            number,
            title: format!("Step {number}"),
            ..SopStep::default()
        }
    }

    fn sop() -> Sop {
        Sop {
            name: "test".into(),
            description: "test".into(),
            version: "0.1.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: Vec::new(),
            steps: vec![step(1), step(2)],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
        }
    }

    fn run() -> SopRun {
        SopRun {
            run_id: "run".into(),
            sop_name: "test".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "now".into(),
            },
            frame_marker_id: "marker-run".into(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 2,
            started_at: "now".into(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
        }
    }

    #[test]
    fn linear_default_routes_to_next_step() {
        let sop = sop();
        let run = run();
        let run_data = RunData::default();
        let ctx = RouteCtx {
            sop: &sop,
            run: &run,
            run_data: &run_data,
            last_status: SopStepStatus::Completed,
            max_step_visits: 256,
        };

        assert_eq!(resolve_next(&ctx), NextStep::Step(2));
    }

    #[test]
    fn dependency_without_output_waits() {
        let mut sop = sop();
        sop.steps[1].routing.depends_on = vec![1];
        let mut run = run();
        run.current_step = 1;
        let run_data = RunData::default();
        let ctx = RouteCtx {
            sop: &sop,
            run: &run,
            run_data: &run_data,
            last_status: SopStepStatus::Completed,
            max_step_visits: 256,
        };

        assert_eq!(resolve_next(&ctx), NextStep::Wait(2));
    }
}
