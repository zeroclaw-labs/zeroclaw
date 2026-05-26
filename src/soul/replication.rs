//! Self-replication lifecycle — state machine for spawning child agents.
//!
//! Enforces max children, constitution propagation with hash verification,
//! and a deterministic lifecycle: Requested → SandboxCreated → RuntimeReady
//! → ConstitutionVerified → Success (or Failed at any point).

use crate::config::ReplicationConfig;
use crate::soul::constitution::Constitution;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplicationPhase {
    Requested,
    SandboxCreated,
    RuntimeReady,
    ConstitutionVerified,
    Success,
    Failed,
}

impl ReplicationPhase {
    fn next(self) -> Option<Self> {
        match self {
            Self::Requested => Some(Self::SandboxCreated),
            Self::SandboxCreated => Some(Self::RuntimeReady),
            Self::RuntimeReady => Some(Self::ConstitutionVerified),
            Self::ConstitutionVerified => Some(Self::Success),
            Self::Success | Self::Failed => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildRecord {
    pub id: String,
    pub phase: ReplicationPhase,
    pub workspace: PathBuf,
    pub error: Option<String>,
}

pub struct ReplicationManager {
    max_children: usize,
    base_workspace: PathBuf,
    children: HashMap<String, ChildRecord>,
    constitution: Option<Constitution>,
}

impl ReplicationManager {
    pub fn new(config: &ReplicationConfig, workspace_dir: &std::path::Path) -> Self {
        Self {
            max_children: config.max_children,
            base_workspace: workspace_dir.join(&config.child_workspace_dir),
            children: HashMap::new(),
            constitution: None,
        }
    }

    pub fn set_constitution(&mut self, constitution: Constitution) {
        self.constitution = Some(constitution);
    }

    pub fn active_children(&self) -> usize {
        self.children
            .values()
            .filter(|c| !c.phase.is_terminal() || c.phase == ReplicationPhase::Success)
            .count()
    }

    pub fn can_spawn(&self) -> bool {
        self.active_children() < self.max_children
    }

    pub fn request_spawn(&mut self, child_id: &str) -> Result<&ChildRecord, ReplicationError> {
        if !self.can_spawn() {
            return Err(ReplicationError::MaxChildrenReached {
                max: self.max_children,
                active: self.active_children(),
            });
        }

        if self.children.contains_key(child_id) {
            return Err(ReplicationError::DuplicateChildId(child_id.to_string()));
        }

        let workspace = self.base_workspace.join(child_id);
        let record = ChildRecord {
            id: child_id.to_string(),
            phase: ReplicationPhase::Requested,
            workspace,
            error: None,
        };

        self.children.insert(child_id.to_string(), record);
        Ok(&self.children[child_id])
    }

    pub fn advance_phase(&mut self, child_id: &str) -> Result<ReplicationPhase, ReplicationError> {
        let record = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| ReplicationError::ChildNotFound(child_id.to_string()))?;

        if record.phase.is_terminal() {
            return Err(ReplicationError::AlreadyTerminal(record.phase));
        }

        let next = record
            .phase
            .next()
            .ok_or(ReplicationError::AlreadyTerminal(record.phase))?;

        record.phase = next;
        Ok(next)
    }

    pub fn fail_child(&mut self, child_id: &str, reason: &str) -> Result<(), ReplicationError> {
        let record = self
            .children
            .get_mut(child_id)
            .ok_or_else(|| ReplicationError::ChildNotFound(child_id.to_string()))?;

        record.phase = ReplicationPhase::Failed;
        record.error = Some(reason.to_string());
        Ok(())
    }

    pub fn verify_constitution(&self, child_hash: &str) -> bool {
        self.constitution
            .as_ref()
            .is_some_and(|c| c.hash() == child_hash)
    }

    pub fn child(&self, id: &str) -> Option<&ChildRecord> {
        self.children.get(id)
    }

    pub fn children(&self) -> impl Iterator<Item = &ChildRecord> {
        self.children.values()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplicationError {
    #[error("max children reached ({active}/{max})")]
    MaxChildrenReached { max: usize, active: usize },

    #[error("duplicate child id: {0}")]
    DuplicateChildId(String),

    #[error("child not found: {0}")]
    ChildNotFound(String),

    #[error("child already in terminal phase: {0:?}")]
    AlreadyTerminal(ReplicationPhase),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ReplicationConfig {
        ReplicationConfig {
            enabled: true,
            max_children: 2,
            child_workspace_dir: "children".into(),
        }
    }

    fn test_manager() -> ReplicationManager {
        ReplicationManager::new(&test_config(), std::path::Path::new("/tmp/zeroclaw"))
    }

    #[test]
    fn spawn_and_advance_lifecycle() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();
        assert_eq!(
            mgr.child("child_1").unwrap().phase,
            ReplicationPhase::Requested
        );

        assert_eq!(
            mgr.advance_phase("child_1").unwrap(),
            ReplicationPhase::SandboxCreated
        );
        assert_eq!(
            mgr.advance_phase("child_1").unwrap(),
            ReplicationPhase::RuntimeReady
        );
        assert_eq!(
            mgr.advance_phase("child_1").unwrap(),
            ReplicationPhase::ConstitutionVerified
        );
        assert_eq!(
            mgr.advance_phase("child_1").unwrap(),
            ReplicationPhase::Success
        );
    }

    #[test]
    fn max_children_enforced() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();
        mgr.request_spawn("child_2").unwrap();

        let err = mgr.request_spawn("child_3").unwrap_err();
        assert!(matches!(err, ReplicationError::MaxChildrenReached { .. }));
    }

    #[test]
    fn duplicate_child_id_rejected() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();

        let err = mgr.request_spawn("child_1").unwrap_err();
        assert!(matches!(err, ReplicationError::DuplicateChildId(_)));
    }

    #[test]
    fn fail_child_sets_terminal() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();
        mgr.fail_child("child_1", "sandbox creation failed")
            .unwrap();

        let child = mgr.child("child_1").unwrap();
        assert_eq!(child.phase, ReplicationPhase::Failed);
        assert_eq!(child.error.as_deref(), Some("sandbox creation failed"));
    }

    #[test]
    fn advance_terminal_returns_error() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();
        mgr.fail_child("child_1", "failed").unwrap();

        let err = mgr.advance_phase("child_1").unwrap_err();
        assert!(matches!(err, ReplicationError::AlreadyTerminal(_)));
    }

    #[test]
    fn constitution_verification() {
        let mut mgr = test_manager();
        let constitution = Constitution::default();
        let hash = constitution.hash().to_string();
        mgr.set_constitution(constitution);

        assert!(mgr.verify_constitution(&hash));
        assert!(!mgr.verify_constitution("wrong_hash"));
    }

    #[test]
    fn failed_children_free_slots() {
        let mut mgr = test_manager();
        mgr.request_spawn("child_1").unwrap();
        mgr.request_spawn("child_2").unwrap();
        assert!(!mgr.can_spawn());

        mgr.fail_child("child_1", "failed").unwrap();
        assert!(mgr.can_spawn());
    }

    #[test]
    fn child_workspace_path() {
        let mgr = test_manager();
        let mut mgr = mgr;
        mgr.request_spawn("agent_a").unwrap();
        let child = mgr.child("agent_a").unwrap();
        assert_eq!(
            child.workspace,
            PathBuf::from("/tmp/zeroclaw/children/agent_a")
        );
    }

    #[test]
    fn phase_next_transitions() {
        assert_eq!(
            ReplicationPhase::Requested.next(),
            Some(ReplicationPhase::SandboxCreated)
        );
        assert_eq!(
            ReplicationPhase::ConstitutionVerified.next(),
            Some(ReplicationPhase::Success)
        );
        assert!(ReplicationPhase::Success.next().is_none());
        assert!(ReplicationPhase::Failed.next().is_none());
    }
}
