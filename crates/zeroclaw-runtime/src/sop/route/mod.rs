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
///
/// Resolution precedence (highest to lowest):
/// 1. Top-level `when` guard — if false, bypasses ALL routing and routes linearly (only linear successor/terminal considered)
/// 2. Switch evaluation — top-to-bottom matching on port `when` guards when `when` is true/None
/// 3. Explicit `next` target (from switch evaluation or declared on step)
/// 4. Linear successor (current_step + 1) when neither switch nor explicit_next provides a target
/// 5. Terminal completion when no successor exists
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

    let payload = ctx.run_data.to_payload().to_string();
    let when_allows_jump = match current.routing.when.as_deref() {
        None => true,
        Some(when) => evaluate_condition(when, Some(&payload)),
    };

    // When top-level `when` guard is false, bypass ALL routing decisions (switch, explicit_next)
    // and route directly to linear successor or terminal completion
    // Restores the linear-successor behavior for a false guard that predates
    // unconditional switch evaluation.
    if !when_allows_jump {
        if current.routing.terminal {
            return NextStep::Complete;
        }
        let next_step = ctx.run.current_step.saturating_add(1);
        let Some(step) = ctx.sop.steps.iter().find(|step| step.number == next_step) else {
            return if next_step > ctx.run.total_steps {
                NextStep::Complete
            } else {
                NextStep::Fail(format!("step {next_step} does not exist"))
            };
        };

        if !guard::within_visit_bound(ctx.run, next_step, ctx.max_step_visits) {
            return NextStep::Fail(format!("step {next_step} visit limit reached"));
        }

        if eligible(step, ctx.run_data) {
            return NextStep::Step(next_step);
        } else {
            return NextStep::Wait(next_step);
        }
    }

    // When guard is true or None: proceed with normal routing decisions
    // Switch evaluation runs first when the guard allows it.
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
        // No switch rules matched, fall through to declared next/linear
    }

    // Check for explicit next target (from routing configuration)
    if let Some(next_target) = current.routing.next {
        if next_target == ctx.run.current_step {
            // Self-loop, but still need to handle terminal case
        }
        let Some(step) = ctx.sop.steps.iter().find(|s| s.number == next_target) else {
            return NextStep::Fail(format!("step {next_target} does not exist"));
        };

        if !guard::within_visit_bound(ctx.run, next_target, ctx.max_step_visits) {
            return NextStep::Fail(format!("step {next_target} visit limit reached"));
        }

        if eligible(step, ctx.run_data) {
            return NextStep::Step(next_target);
        } else {
            return NextStep::Wait(next_target);
        }
    }

    // No explicit next, no switch matched: use linear successor
    if current.routing.terminal {
        return NextStep::Complete;
    }

    let next_step = ctx.run.current_step.saturating_add(1);
    let Some(step) = ctx.sop.steps.iter().find(|step| step.number == next_step) else {
        return if next_step > ctx.run.total_steps {
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
    use crate::sop::{
        SwitchRule,
        types::{SopEvent, SopExecutionMode, SopPriority, SopRunStatus, SopTriggerSource},
    };

    fn step(number: u32) -> SopStep {
        SopStep {
            number,
            title: format!("Step {number}"),
            ..SopStep::default()
        }
    }

    fn sop_with_steps(steps: Vec<SopStep>) -> Sop {
        Sop {
            name: "test".into(),
            description: "test".into(),
            version: "0.1.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: Vec::new(),
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
        }
    }

    fn sop() -> Sop {
        sop_with_steps(vec![step(1), step(2)])
    }

    fn run_at(current_step: u32, total_steps: u32) -> SopRun {
        let mut run = run();
        run.current_step = current_step;
        run.total_steps = total_steps;
        run
    }

    fn loop_step(number: u32) -> SopStep {
        let mut loop_step = step(number);
        loop_step.routing.when = Some(format!("$.steps.{number}.remaining > 0"));
        loop_step.routing.next = Some(number);
        loop_step
    }

    fn run_data_with_remaining(step_number: u32, remaining: u32) -> RunData {
        let mut run_data = RunData::default();
        run_data.insert_output_str(step_number, &format!(r#"{{"remaining":{remaining}}}"#));
        run_data
    }

    fn route_ctx<'a>(sop: &'a Sop, run: &'a SopRun, run_data: &'a RunData) -> RouteCtx<'a> {
        RouteCtx {
            sop,
            run,
            run_data,
            last_status: SopStepStatus::Completed,
            max_step_visits: 256,
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

    #[test]
    fn when_true_self_loop_routes_back() {
        let sop = sop_with_steps(vec![step(1), loop_step(2), step(3)]);
        let run = run_at(2, 3);
        let run_data = run_data_with_remaining(2, 1);
        let ctx = route_ctx(&sop, &run, &run_data);

        assert_eq!(resolve_next(&ctx), NextStep::Step(2));
    }

    #[test]
    fn when_false_advances_to_linear_successor() {
        let sop = sop_with_steps(vec![step(1), loop_step(2), step(3)]);
        let run = run_at(2, 3);
        let run_data = run_data_with_remaining(2, 0);
        let ctx = route_ctx(&sop, &run, &run_data);

        // When `when` is false, it should bypass all routing and go to linear successor (step 3)
        // The loop_step has `next = Some(2)` but false `when` ignores it
        assert_eq!(resolve_next(&ctx), NextStep::Step(3));
    }

    #[test]
    fn when_false_at_end_completes() {
        let sop = sop_with_steps(vec![step(1), loop_step(2)]);
        let run = run_at(2, 2);
        let run_data = run_data_with_remaining(2, 0);
        let ctx = route_ctx(&sop, &run, &run_data);

        // When `when` is false at the last step with terminal=false, it should complete
        // because linear successor (3) > total_steps (2)
        assert_eq!(resolve_next(&ctx), NextStep::Complete);
    }

    // Regression test: false top-level `when` should bypass switch to linear.
    #[test]
    fn when_false_bypasses_switch_to_linear_successor() {
        let switch_rules = vec![
            SwitchRule {
                name: "target2".to_string(),
                when: Some("$.steps.1.enabled == true".to_string()),
                goto: Some(2),
            },
            SwitchRule {
                name: "target3".to_string(),
                when: None, // catch-all goes to step 3
                goto: Some(3),
            },
        ];

        // Step 1 has a false `when` condition and a switch with two ports
        // It should bypass the switch evaluation entirely and go to linear successor (step 2 = 1+1)
        let mut step1 = step(1);
        step1.routing.when = Some("$.steps.1.enabled == false".to_string());
        step1.routing.switch = switch_rules;

        let sop = sop_with_steps(vec![step1, step(2), step(3)]);
        let run = run_at(1, 3);
        let run_data = RunData::default(); // No outputs, making when condition false
        let ctx = route_ctx(&sop, &run, &run_data);

        // Should go to linear successor (step 2 = 1+1), NOT switch target
        assert_eq!(resolve_next(&ctx), NextStep::Step(2));
    }

    // Inverse test: true top-level `when` with switch should evaluate based on when + switch
    #[test]
    fn when_true_evaluates_switch_correctly() {
        // Use distinct steps: switch targets are 3 and 4, linear successor is 2
        // This ensures the test catches bugs where wrong path is taken
        let switch_rules = vec![
            SwitchRule {
                name: "target3".to_string(),
                when: Some("$.steps.1.enabled == true".to_string()),
                goto: Some(3),
            },
            SwitchRule {
                name: "target4".to_string(),
                when: None, // catch-all goes to step 4
                goto: Some(4),
            },
        ];

        // Step 1 has a true `when` condition and a switch with two ports
        let mut step1 = step(1);
        let mut run_data = RunData::default();
        run_data.insert_output_str(1, r#"{"enabled": true}"#);
        step1.routing.when = Some("$.steps.1.enabled == true".to_string());
        step1.routing.switch = switch_rules;

        let sop = sop_with_steps(vec![step1, step(2), step(3), step(4)]);
        let run = run_at(1, 4);
        let ctx = route_ctx(&sop, &run, &run_data);

        // Should use the FIRST matching switch port (target3 = step 3), NOT linear successor (2)
        assert_eq!(resolve_next(&ctx), NextStep::Step(3));
    }
}
