use serde::{Deserialize, Serialize};

use super::graph::FlowRole;
use super::step_contract::{StepFailure, SwitchRule};
use super::types::Sop;

/// Whether an edge operation adds or removes the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOp {
    Connect,
    Disconnect,
}

/// A single edge mutation request from an authoring surface. `role` is the
/// canonical edge kind (`FlowRole`); `port` indexes the source step's switch
/// rule and is required only for `FlowRole::Switch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEdit {
    pub op: WireOp,
    pub from: u32,
    pub to: u32,
    pub role: FlowRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<usize>,
}

/// Why an edge mutation could not be applied. Surfaces render the message; the
/// SOP is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    UnknownStep(u32),
    MissingPort,
    PortOutOfRange(usize),
    SelfLoop(u32),
    TriggerNotWirable,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStep(n) => write!(f, "step {n} does not exist"),
            Self::MissingPort => write!(f, "switch edge requires a port index"),
            Self::PortOutOfRange(i) => write!(f, "switch port {i} out of range"),
            Self::SelfLoop(n) => write!(f, "step {n} cannot wire to itself"),
            Self::TriggerNotWirable => write!(
                f,
                "trigger edges are derived from the SOP's triggers and cannot be wired by hand"
            ),
        }
    }
}

impl std::error::Error for WireError {}

/// Apply one edge mutation to a SOP in place. This is the single source of
/// truth for how each `FlowRole` maps onto step routing / failure policy;
/// every authoring surface routes edge CRUD through here rather than mutating
/// routing fields itself.
pub fn apply_wire(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    if edit.role == FlowRole::Trigger {
        return Err(WireError::TriggerNotWirable);
    }
    if !sop.steps.iter().any(|s| s.number == edit.from) {
        return Err(WireError::UnknownStep(edit.from));
    }
    if !sop.steps.iter().any(|s| s.number == edit.to) {
        return Err(WireError::UnknownStep(edit.to));
    }
    if edit.from == edit.to {
        return Err(WireError::SelfLoop(edit.from));
    }
    match edit.role {
        FlowRole::Sequence => apply_sequence(sop, edit),
        FlowRole::Dependency => apply_dependency(sop, edit),
        FlowRole::Failure => apply_failure(sop, edit),
        FlowRole::Switch => apply_switch(sop, edit),
        FlowRole::Trigger => Err(WireError::TriggerNotWirable),
    }
}

fn step_mut(sop: &mut Sop, number: u32) -> &mut super::types::SopStep {
    sop.steps
        .iter_mut()
        .find(|s| s.number == number)
        .expect("caller verified the step exists")
}

fn apply_sequence(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    let step = step_mut(sop, edit.from);
    match edit.op {
        WireOp::Connect => step.routing.next = Some(edit.to),
        WireOp::Disconnect => {
            if step.routing.next == Some(edit.to) {
                step.routing.next = None;
            }
        }
    }
    Ok(())
}

fn apply_dependency(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    let step = step_mut(sop, edit.to);
    match edit.op {
        WireOp::Connect => {
            if !step.routing.depends_on.contains(&edit.from) {
                step.routing.depends_on.push(edit.from);
            }
        }
        WireOp::Disconnect => step.routing.depends_on.retain(|d| *d != edit.from),
    }
    Ok(())
}

fn apply_failure(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    let step = step_mut(sop, edit.from);
    match edit.op {
        WireOp::Connect => step.on_failure = StepFailure::Goto { step: edit.to },
        WireOp::Disconnect => {
            if matches!(step.on_failure, StepFailure::Goto { step } if step == edit.to) {
                step.on_failure = StepFailure::Fail;
            }
        }
    }
    Ok(())
}

fn apply_switch(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    let port = edit.port.ok_or(WireError::MissingPort)?;
    let step = step_mut(sop, edit.from);
    match edit.op {
        WireOp::Connect => {
            while step.routing.switch.len() <= port {
                step.routing.switch.push(SwitchRule::default());
            }
            let rule = step
                .routing
                .switch
                .get_mut(port)
                .ok_or(WireError::PortOutOfRange(port))?;
            rule.goto = Some(edit.to);
        }
        WireOp::Disconnect => {
            let rule = step
                .routing
                .switch
                .get_mut(port)
                .ok_or(WireError::PortOutOfRange(port))?;
            if rule.goto == Some(edit.to) {
                rule.goto = None;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{Sop, SopExecutionMode, SopPriority, SopStep, SopStepKind, SopTrigger};

    fn sop_with(n: u32) -> Sop {
        let steps = (1..=n)
            .map(|i| SopStep {
                number: i,
                title: format!("s{i}"),
                body: format!("b{i}"),
                kind: SopStepKind::Execute,
                ..SopStep::default()
            })
            .collect();
        Sop {
            name: "t".into(),
            description: String::new(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        }
    }

    fn edit(op: WireOp, from: u32, to: u32, role: FlowRole, port: Option<usize>) -> WireEdit {
        WireEdit {
            op,
            from,
            to,
            role,
            port,
        }
    }

    #[test]
    fn sequence_connect_and_disconnect() {
        let mut sop = sop_with(2);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 2, FlowRole::Sequence, None),
        )
        .unwrap();
        assert_eq!(sop.steps[0].routing.next, Some(2));
        apply_wire(
            &mut sop,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Sequence, None),
        )
        .unwrap();
        assert_eq!(sop.steps[0].routing.next, None);
    }

    #[test]
    fn dependency_dedupes_and_removes() {
        let mut sop = sop_with(2);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 2, FlowRole::Dependency, None),
        )
        .unwrap();
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 2, FlowRole::Dependency, None),
        )
        .unwrap();
        assert_eq!(sop.steps[1].routing.depends_on, vec![1]);
        apply_wire(
            &mut sop,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Dependency, None),
        )
        .unwrap();
        assert!(sop.steps[1].routing.depends_on.is_empty());
    }

    #[test]
    fn failure_goto_sets_then_resets_to_fail() {
        let mut sop = sop_with(3);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 3, FlowRole::Failure, None),
        )
        .unwrap();
        assert_eq!(sop.steps[0].on_failure, StepFailure::Goto { step: 3 });
        apply_wire(
            &mut sop,
            &edit(WireOp::Disconnect, 1, 3, FlowRole::Failure, None),
        )
        .unwrap();
        assert_eq!(sop.steps[0].on_failure, StepFailure::Fail);
    }

    #[test]
    fn failure_disconnect_only_clears_matching_target() {
        let mut sop = sop_with(3);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 3, FlowRole::Failure, None),
        )
        .unwrap();
        apply_wire(
            &mut sop,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Failure, None),
        )
        .unwrap();
        assert_eq!(sop.steps[0].on_failure, StepFailure::Goto { step: 3 });
    }

    #[test]
    fn switch_connect_grows_ports_and_sets_goto() {
        let mut sop = sop_with(3);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 3, FlowRole::Switch, Some(1)),
        )
        .unwrap();
        assert_eq!(sop.steps[0].routing.switch.len(), 2);
        assert_eq!(sop.steps[0].routing.switch[1].goto, Some(3));
    }

    #[test]
    fn switch_disconnect_clears_goto_keeps_rule() {
        let mut sop = sop_with(2);
        apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 2, FlowRole::Switch, Some(0)),
        )
        .unwrap();
        apply_wire(
            &mut sop,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Switch, Some(0)),
        )
        .unwrap();
        assert_eq!(sop.steps[0].routing.switch.len(), 1);
        assert_eq!(sop.steps[0].routing.switch[0].goto, None);
    }

    #[test]
    fn switch_requires_port() {
        let mut sop = sop_with(2);
        let err = apply_wire(
            &mut sop,
            &edit(WireOp::Connect, 1, 2, FlowRole::Switch, None),
        );
        assert_eq!(err, Err(WireError::MissingPort));
    }

    #[test]
    fn unknown_step_and_self_loop_rejected() {
        let mut sop = sop_with(2);
        assert_eq!(
            apply_wire(
                &mut sop,
                &edit(WireOp::Connect, 1, 9, FlowRole::Sequence, None)
            ),
            Err(WireError::UnknownStep(9))
        );
        assert_eq!(
            apply_wire(
                &mut sop,
                &edit(WireOp::Connect, 1, 1, FlowRole::Sequence, None)
            ),
            Err(WireError::SelfLoop(1))
        );
    }

    #[test]
    fn trigger_edges_are_rejected() {
        let mut sop = sop_with(2);
        assert_eq!(
            apply_wire(
                &mut sop,
                &edit(WireOp::Connect, 1, 2, FlowRole::Trigger, None)
            ),
            Err(WireError::TriggerNotWirable)
        );
    }
}
