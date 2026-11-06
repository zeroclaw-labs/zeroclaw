//! Admission policy for operator answers to shared cost-limit override prompts.
//!
//! The transport supplies peer identity and parent-process lookup; this module
//! decides whether that evidence admits an answer. Grant state and period
//! accounting remain owned by the shared [`super::CostTracker`].

use std::sync::Arc;

/// Whether an answer to a cost-override prompt may proceed to resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostOverrideAnswer {
    /// The answer came through a local transport from an independent process.
    Proceed,
    /// The evidence does not establish an independent local operator.
    Refuse,
}

/// Admit cost-override answers only from independent local operator processes.
pub fn cost_override_answer(
    is_local_transport: bool,
    independent_peer: bool,
) -> CostOverrideAnswer {
    if is_local_transport && independent_peer {
        CostOverrideAnswer::Proceed
    } else {
        CostOverrideAnswer::Refuse
    }
}

/// Check whether a peer's visible ancestry is independent of the daemon.
///
/// The caller supplies OS support and a parent lookup for the current process
/// tree. Unknown or zero PIDs, unsupported systems, missing parents, daemon
/// ancestry, and walks that do not reach PID 1 within 128 steps fail closed.
/// Reparenting can hide former ancestry; this bounded check is not OS isolation.
pub fn independent_operator_process(
    peer: Option<u32>,
    daemon: u32,
    supported: bool,
    mut parent: impl FnMut(u32) -> Option<u32>,
) -> bool {
    let Some(mut peer) = peer.filter(|pid| *pid > 0) else {
        return false;
    };
    if !supported {
        return false;
    }
    for _ in 0..128 {
        if peer == daemon || peer == 0 {
            return false;
        }
        // PID 1 is the root of the visible process tree.
        if peer == 1 {
            return true;
        }
        let Some(next) = parent(peer) else {
            return false;
        };
        if next == 0 {
            return false;
        }
        peer = next;
    }
    false
}

/// The operator surface that received a cost-override prompt, if any.
///
/// This binding is the source of truth for whether a pending answer is a cost
/// override. Bound entries admit only the exact `Arc` supplied at registration;
/// unbound entries preserve ordinary approval behavior and admit any surface.
pub struct OperatorBinding<S>(Option<Arc<S>>);

impl<S> OperatorBinding<S> {
    /// Bind to the surface that received the prompt, or leave an entry unbound.
    pub fn new(surface: Option<Arc<S>>) -> Self {
        Self(surface)
    }

    /// Whether the entry is bound to an operator surface for a cost override.
    pub fn is_bound(&self) -> bool {
        self.0.is_some()
    }

    /// Admit an exact bound surface, or any/no surface for an unbound entry.
    pub fn admits(&self, actual: Option<&Arc<S>>) -> bool {
        self.0
            .as_ref()
            .is_none_or(|expected| actual.is_some_and(|actual| Arc::ptr_eq(expected, actual)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_override_rejects_daemon_descendants_and_unknown_ancestry() {
        let parent = |pid| match pid {
            40 => Some(30),
            30 => Some(20),
            20 => Some(1),
            50 => Some(1),
            _ => None,
        };
        assert!(!independent_operator_process(Some(20), 20, true, parent));
        assert!(!independent_operator_process(Some(30), 20, true, parent));
        assert!(!independent_operator_process(Some(40), 20, true, parent));
        assert!(independent_operator_process(Some(50), 20, true, parent));
        assert!(!independent_operator_process(Some(99), 20, true, parent));
        assert!(!independent_operator_process(Some(99), 20, true, Some));
        assert!(!independent_operator_process(None, 20, true, parent));
    }

    #[test]
    fn cost_override_rejects_zero_pids_and_unsupported_lookups() {
        let no_lookup = |_| panic!("invalid peers and unsupported systems must skip lookup");
        assert!(!independent_operator_process(Some(0), 20, true, no_lookup));
        assert!(!independent_operator_process(None, 20, true, no_lookup));
        assert!(!independent_operator_process(
            Some(50),
            20,
            false,
            no_lookup
        ));
        assert!(!independent_operator_process(Some(50), 20, true, |_| Some(
            0
        )));
        assert!(!independent_operator_process(Some(1), 1, true, no_lookup));
        assert!(independent_operator_process(Some(1), 20, true, no_lookup));
    }

    #[test]
    fn cost_override_rejects_cycles_and_over_depth_ancestry() {
        let cycle = |pid| match pid {
            50 => Some(60),
            60 => Some(50),
            _ => None,
        };
        assert!(!independent_operator_process(Some(50), 20, true, cycle));
        let chain = |pid: u32| pid.checked_sub(1);
        assert!(independent_operator_process(Some(128), 500, true, chain));
        assert!(!independent_operator_process(Some(129), 500, true, chain));
    }

    #[test]
    fn cost_override_answers_require_local_transport_and_independence() {
        assert_eq!(
            cost_override_answer(true, true),
            CostOverrideAnswer::Proceed
        );
        assert_eq!(
            cost_override_answer(true, false),
            CostOverrideAnswer::Refuse
        );
        assert_eq!(
            cost_override_answer(false, true),
            CostOverrideAnswer::Refuse
        );
        assert_eq!(
            cost_override_answer(false, false),
            CostOverrideAnswer::Refuse
        );
    }

    #[test]
    fn operator_binding_requires_the_exact_surface_only_when_bound() {
        let intended = Arc::new(String::from("operator"));
        let same_surface = Arc::clone(&intended);
        let other = Arc::new(String::from("operator"));
        let bound = OperatorBinding::new(Some(Arc::clone(&intended)));
        assert!(bound.is_bound());
        assert!(bound.admits(Some(&intended)));
        assert!(bound.admits(Some(&same_surface)));
        assert!(!bound.admits(Some(&other)));
        assert!(!bound.admits(None));

        let unbound = OperatorBinding::<String>::new(None);
        assert!(!unbound.is_bound());
        assert!(unbound.admits(Some(&intended)));
        assert!(unbound.admits(Some(&other)));
        assert!(unbound.admits(None));
    }
}
