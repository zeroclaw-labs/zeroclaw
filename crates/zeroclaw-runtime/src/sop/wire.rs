use serde::{Deserialize, Serialize};

use super::graph::FlowRole;
use super::step_contract::{StepFailure, SwitchRule};
use super::types::Sop;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOp {
    Connect,
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEdit {
    pub op: WireOp,
    pub from: u32,
    pub to: u32,
    pub role: FlowRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<usize>,
}

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
        WireOp::Connect => {
            step.routing.next = Some(edit.to);
            step.routing.terminal = false;
        }
        WireOp::Disconnect => {
            if step.routing.next == Some(edit.to) {
                step.routing.next = None;
            }
            step.routing.terminal = true;
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
