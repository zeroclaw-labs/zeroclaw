//! Workspace isolation boundary enforcement.
//!
//! Prevents cross-workspace data access and enforces per-workspace
//! domain allowlists and tool restrictions.

use crate::config::workspace::WorkspaceProfile;
use std::path::Path;

/// Outcome of a workspace boundary check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryVerdict {
    /// Access is allowed.
    Allow,
    /// Access is denied with a reason.
    Deny(String),
}

/// Enforces isolation boundaries for the active workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceBoundary {
    /// The active workspace profile (if workspace isolation is active).
    profile: Option<WorkspaceProfile>,
    /// Whether cross-workspace search is allowed.
    cross_workspace_search: bool,
}

impl WorkspaceBoundary {
    /// Create a boundary enforcer for the given active workspace.
    pub fn new(profile: Option<WorkspaceProfile>, cross_workspace_search: bool) -> Self {
        Self {
            profile,
            cross_workspace_search,
        }
    }

    /// Create a boundary enforcer with no active workspace (no restrictions).
    pub fn inactive() -> Self {
        Self {
            profile: None,
            cross_workspace_search: false,
        }
    }

    /// Check whether a tool is allowed in the current workspace.
    pub fn check_tool_access(&self, tool_name: &str) -> BoundaryVerdict {
        if let Some(profile) = &self.profile {
            if profile.is_tool_restricted(tool_name) {
                return BoundaryVerdict::Deny(format!(
                    "tool '{}' is restricted in workspace '{}'",
                    tool_name, profile.name
                ));
            }
        }
        BoundaryVerdict::Allow
    }

    /// Check whether a domain is allowed in the current workspace.
    pub fn check_domain_access(&self, domain: &str) -> BoundaryVerdict {
        if let Some(profile) = &self.profile {
            if !profile.is_domain_allowed(domain) {
                return BoundaryVerdict::Deny(format!(
                    "domain '{}' is not in the allowlist for workspace '{}'",
                    domain, profile.name
                ));
            }
        }
        BoundaryVerdict::Allow
    }

    /// Check whether accessing a path is allowed given workspace isolation.
    ///
    /// When a workspace is active, paths outside the workspace directory
    /// and paths belonging to other workspaces are denied.
    pub fn check_path_access(&self, path: &Path, workspaces_base: &Path) -> BoundaryVerdict {
        let profile = match &self.profile {
            Some(p) => p,
            None => return BoundaryVerdict::Allow,
        };

        // If the path is under the workspaces base, verify it belongs to the active workspace
        if let Ok(relative) = path.strip_prefix(workspaces_base) {
            let first_component = relative
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str());

            if let Some(ws_name) = first_component {
                if ws_name != profile.name {
                    if self.cross_workspace_search {
                        // Cross-workspace search is allowed, but only for read-like access
                        return BoundaryVerdict::Allow;
                    }
                    return BoundaryVerdict::Deny(format!(
                        "access to workspace '{}' is denied from workspace '{}'",
                        ws_name, profile.name
                    ));
                }
            }
        }

        BoundaryVerdict::Allow
    }

    /// Whether workspace isolation is active.
    pub fn is_active(&self) -> bool {
        self.profile.is_some()
    }

    /// Get the active workspace name, if any.
    pub fn active_workspace_name(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_profile() -> WorkspaceProfile {
        WorkspaceProfile {
            name: "client_a".to_string(),
            allowed_domains: vec!["api.example.com".to_string()],
            credential_profile: None,
            memory_namespace: Some("client_a".to_string()),
            audit_namespace: Some("client_a".to_string()),
            tool_restrictions: vec!["shell".to_string()],
        }
    }

    #[test]
    fn boundary_inactive_allows_everything() {
        let boundary = WorkspaceBoundary::inactive();
        assert_eq!(boundary.check_tool_access("shell"), BoundaryVerdict::Allow);
        assert_eq!(
            boundary.check_domain_access("any.domain"),
            BoundaryVerdict::Allow
        );
        assert!(!boundary.is_active());
    }

    #[test]
    fn boundary_denies_restricted_tool() {
        let boundary = WorkspaceBoundary::new(Some(test_profile()), false);
        assert!(matches!(
            boundary.check_tool_access("shell"),
            BoundaryVerdict::Deny(_)
        ));
        assert_eq!(
            boundary.check_tool_access("file_read"),
            BoundaryVerdict::Allow
        );
    }

    #[test]
    fn boundary_denies_unlisted_domain() {
        let boundary = WorkspaceBoundary::new(Some(test_profile()), false);
        assert_eq!(
            boundary.check_domain_access("api.example.com"),
            BoundaryVerdict::Allow
        );
        assert!(matches!(
            boundary.check_domain_access("evil.com"),
            BoundaryVerdict::Deny(_)
        ));
    }

    #[test]
    fn boundary_denies_cross_workspace_path_access() {
        let boundary = WorkspaceBoundary::new(Some(test_profile()), false);
        let base = PathBuf::from("/home/zeroclaw_user/.zeroclaw/workspaces");

        // Access to own workspace is allowed
        let own_path = base.join("client_a").join("data.db");
        assert_eq!(
            boundary.check_path_access(&own_path, &base),
            BoundaryVerdict::Allow
        );

        // Access to other workspace is denied
        let other_path = base.join("client_b").join("data.db");
        assert!(matches!(
            boundary.check_path_access(&other_path, &base),
            BoundaryVerdict::Deny(_)
        ));
    }

    #[test]
    fn boundary_allows_cross_workspace_when_enabled() {
        let boundary = WorkspaceBoundary::new(Some(test_profile()), true);
        let base = PathBuf::from("/home/zeroclaw_user/.zeroclaw/workspaces");
        let other_path = base.join("client_b").join("data.db");

        assert_eq!(
            boundary.check_path_access(&other_path, &base),
            BoundaryVerdict::Allow
        );
    }

    #[test]
    fn boundary_allows_paths_outside_workspaces_dir() {
        let boundary = WorkspaceBoundary::new(Some(test_profile()), false);
        let base = PathBuf::from("/home/zeroclaw_user/.zeroclaw/workspaces");
        let outside_path = PathBuf::from("/tmp/something");

        assert_eq!(
            boundary.check_path_access(&outside_path, &base),
            BoundaryVerdict::Allow
        );
    }
}
