//! Wire-edit application: translates a visual editor's connect/disconnect
//! gesture into the corresponding `StepRouting`/`StepFailure` mutation on a
//! draft `Sop`. The graph is a projection; these edits write back to the
//! source of truth it was projected from.

use serde::{Deserialize, Serialize};

use super::graph::FlowRole;
use super::step_contract::{StepFailure, SwitchRule};
use super::types::Sop;

const MAX_SWITCH_PORTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOp {
    /// Create the edge, overwriting any previous target of the same role.
    Connect,
    /// Remove the edge only if it currently points at the given target.
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One editor gesture. `role` picks which routing field is mutated; `port`
/// is required for `Switch` (index into `routing.switch`) and ignored
/// otherwise. Accepted over RPC (`sops/wire-draft`) and HTTP.
pub struct WireEdit {
    pub op: WireOp,
    /// Source step number (the node the wire leaves).
    pub from: u32,
    /// Target step number (the node the wire enters).
    pub to: u32,
    pub role: FlowRole,
    /// Switch port index; grows the port list on connect if needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Why a wire edit was rejected. The draft SOP is left untouched on error.
pub enum WireError {
    /// `from` or `to` names a step number not present in the SOP.
    UnknownStep(u32),
    /// `Switch` edit without a `port` index.
    MissingPort,
    /// `port` does not exist on the step (disconnect never grows the list).
    PortOutOfRange(usize),
    /// `from == to`; steps cannot wire to themselves.
    SelfLoop(u32),
    /// Trigger edges are derived from `sop.triggers`, never hand-wired.
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

/// Apply one wire edit to a draft SOP, mutating the routing field the edit's
/// role maps to. Validates both endpoints first; on `Err` the SOP is
/// unchanged. Semantics per role:
///
/// - `Sequence` connect sets `routing.next` and clears `terminal`;
///   disconnect clears a matching `next` and sets `terminal` so the implicit
///   fallthrough edge does not immediately reappear.
/// - `Dependency` edits `to`'s `depends_on` (connect is idempotent).
/// - `Failure` sets/clears `on_failure: goto` on `from`.
/// - `Switch` sets/clears `routing.switch[port].goto` on `from`; connect
///   grows the port list with default rules up to `port`.
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

fn step_mut(sop: &mut Sop, number: u32) -> Result<&mut super::types::SopStep, WireError> {
    sop.steps
        .iter_mut()
        .find(|s| s.number == number)
        .ok_or(WireError::UnknownStep(number))
}

fn apply_sequence(sop: &mut Sop, edit: &WireEdit) -> Result<(), WireError> {
    let step = step_mut(sop, edit.from)?;
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
    let step = step_mut(sop, edit.to)?;
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
    let step = step_mut(sop, edit.from)?;
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
    if port >= MAX_SWITCH_PORTS {
        return Err(WireError::PortOutOfRange(port));
    }
    let step = step_mut(sop, edit.from)?;
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
    use crate::sop::step_contract::StepRouting;
    use crate::sop::types::{Sop, SopExecutionMode, SopPriority, SopStep};

    fn sop2() -> Sop {
        Sop {
            name: "w".into(),
            description: String::new(),
            version: "0.1.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: Vec::new(),
            steps: vec![
                SopStep {
                    number: 1,
                    title: "a".into(),
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "b".into(),
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: Default::default(),
            max_pending_approvals: 0,
            agent: None,
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

    fn step(sop: &Sop, n: u32) -> &SopStep {
        sop.steps.iter().find(|s| s.number == n).unwrap()
    }

    #[test]
    fn sequence_connect_sets_next_and_clears_terminal() {
        let mut s = sop2();
        s.steps[0].routing.terminal = true;
        apply_wire(
            &mut s,
            &edit(WireOp::Connect, 1, 2, FlowRole::Sequence, None),
        )
        .unwrap();
        assert_eq!(step(&s, 1).routing.next, Some(2));
        assert!(!step(&s, 1).routing.terminal);
    }

    #[test]
    fn sequence_disconnect_clears_matching_next_and_marks_terminal() {
        let mut s = sop2();
        s.steps[0].routing.next = Some(2);
        apply_wire(
            &mut s,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Sequence, None),
        )
        .unwrap();
        assert_eq!(step(&s, 1).routing.next, None);
        assert!(
            step(&s, 1).routing.terminal,
            "disconnect must suppress implicit fallthrough, not just clear next"
        );
    }

    #[test]
    fn dependency_connect_is_idempotent_and_disconnect_removes() {
        let mut s = sop2();
        let e = edit(WireOp::Connect, 1, 2, FlowRole::Dependency, None);
        apply_wire(&mut s, &e).unwrap();
        apply_wire(&mut s, &e).unwrap();
        assert_eq!(step(&s, 2).routing.depends_on, vec![1]);

        apply_wire(
            &mut s,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Dependency, None),
        )
        .unwrap();
        assert!(step(&s, 2).routing.depends_on.is_empty());
    }

    #[test]
    fn failure_disconnect_only_clears_matching_target() {
        let mut s = sop2();
        s.steps[0].on_failure = StepFailure::Goto { step: 2 };
        // Disconnecting an edge to a different target must not touch it. Step 1
        // exists as `to`, so validation passes but the goto doesn't match.
        apply_wire(
            &mut s,
            &edit(WireOp::Disconnect, 2, 1, FlowRole::Failure, None),
        )
        .unwrap();
        assert_eq!(step(&s, 1).on_failure, StepFailure::Goto { step: 2 });

        apply_wire(
            &mut s,
            &edit(WireOp::Disconnect, 1, 2, FlowRole::Failure, None),
        )
        .unwrap();
        assert_eq!(step(&s, 1).on_failure, StepFailure::Fail);
    }

    #[test]
    fn switch_connect_grows_ports_and_requires_port_index() {
        let mut s = sop2();
        assert_eq!(
            apply_wire(&mut s, &edit(WireOp::Connect, 1, 2, FlowRole::Switch, None)),
            Err(WireError::MissingPort)
        );

        apply_wire(
            &mut s,
            &edit(WireOp::Connect, 1, 2, FlowRole::Switch, Some(1)),
        )
        .unwrap();
        assert_eq!(step(&s, 1).routing.switch.len(), 2);
        assert_eq!(step(&s, 1).routing.switch[1].goto, Some(2));
        assert_eq!(step(&s, 1).routing.switch[0].goto, None);
    }

    #[test]
    fn switch_disconnect_out_of_range_port_errors() {
        let mut s = sop2();
        assert_eq!(
            apply_wire(
                &mut s,
                &edit(WireOp::Disconnect, 1, 2, FlowRole::Switch, Some(3))
            ),
            Err(WireError::PortOutOfRange(3))
        );
    }

    #[test]
    fn rejects_unknown_steps_self_loops_and_trigger_edges() {
        let mut s = sop2();
        assert_eq!(
            apply_wire(
                &mut s,
                &edit(WireOp::Connect, 9, 2, FlowRole::Sequence, None)
            ),
            Err(WireError::UnknownStep(9))
        );
        assert_eq!(
            apply_wire(
                &mut s,
                &edit(WireOp::Connect, 1, 9, FlowRole::Sequence, None)
            ),
            Err(WireError::UnknownStep(9))
        );
        assert_eq!(
            apply_wire(
                &mut s,
                &edit(WireOp::Connect, 1, 1, FlowRole::Sequence, None)
            ),
            Err(WireError::SelfLoop(1))
        );
        assert_eq!(
            apply_wire(
                &mut s,
                &edit(WireOp::Connect, 1, 2, FlowRole::Trigger, None)
            ),
            Err(WireError::TriggerNotWirable)
        );
        assert_eq!(step(&s, 1).routing, StepRouting::default());
    }
}
