//! IAM-aware policy enforcement for Nevis role-to-permission mapping.
//!
//! Evaluates tool and workspace access based on Nevis roles using a
//! deny-by-default policy model. All policy decisions are audit-logged.

use super::nevis::NevisIdentity;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maps a single Nevis role to JhedaiClaw permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleMapping {
    /// Nevis role name (case-insensitive matching).
    pub nevis_role: String,
    /// Tool names this role can access. Use `"all"` to grant all tools.
    pub jhedaiclaw_permissions: Vec<String>,
    /// Workspace names this role can access. Use `"all"` for unrestricted.
    #[serde(default)]
    pub workspace_access: Vec<String>,
}

/// Result of a policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Access is allowed.
    Allow,
    /// Access is denied, with reason.
    Deny(String),
}

impl PolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }
}

/// IAM policy engine that maps Nevis roles to JhedaiClaw tool permissions.
///
/// Deny-by-default: if no role mapping grants access, the request is denied.
#[derive(Debug, Clone)]
pub struct IamPolicy {
    /// Compiled role mappings indexed by lowercase Nevis role name.
    role_map: HashMap<String, CompiledRole>,
}

#[derive(Debug, Clone)]
struct CompiledRole {
    /// Whether this role has access to all tools.
    all_tools: bool,
    /// Specific tool names this role can access (lowercase).
    allowed_tools: Vec<String>,
    /// Whether this role has access to all workspaces.
    all_workspaces: bool,
    /// Specific workspace names this role can access (lowercase).
    allowed_workspaces: Vec<String>,
}

impl IamPolicy {
    /// Build a policy from role mappings (typically from config).
    ///
    /// Returns an error if duplicate normalized role names are detected,
    /// since silent last-wins overwrites can accidentally broaden or revoke access.
    pub fn from_mappings(mappings: &[RoleMapping]) -> Result<Self> {
        let mut role_map = HashMap::new();

        for mapping in mappings {
            let key = mapping.nevis_role.trim().to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }

            let all_tools = mapping
                .jhedaiclaw_permissions
                .iter()
                .any(|p| p.eq_ignore_ascii_case("all"));
            let allowed_tools: Vec<String> = mapping
                .jhedaiclaw_permissions
                .iter()
                .filter(|p| !p.eq_ignore_ascii_case("all"))
                .map(|p| p.trim().to_ascii_lowercase())
                .collect();

            let all_workspaces = mapping
                .workspace_access
                .iter()
                .any(|w| w.eq_ignore_ascii_case("all"));
            let allowed_workspaces: Vec<String> = mapping
                .workspace_access
                .iter()
                .filter(|w| !w.eq_ignore_ascii_case("all"))
                .map(|w| w.trim().to_ascii_lowercase())
                .collect();

            if role_map.contains_key(&key) {
                bail!(
                    "IAM policy: duplicate role mapping for normalized key '{}' \
                     (from nevis_role '{}') — remove or merge the duplicate entry",
                    key,
                    mapping.nevis_role
                );
            }

            role_map.insert(
                key,
                CompiledRole {
                    all_tools,
                    allowed_tools,
                    all_workspaces,
                    allowed_workspaces,
                },
            );
        }

        Ok(Self { role_map })
    }

    /// Evaluate whether an identity is allowed to use a specific tool.
    ///
    /// Deny-by-default: returns `Deny` unless at least one of the identity's
    /// roles grants access to the requested tool.
    pub fn evaluate_tool_access(
        &self,
        identity: &NevisIdentity,
        tool_name: &str,
    ) -> PolicyDecision {
        let normalized_tool = tool_name.trim().to_ascii_lowercase();
        if normalized_tool.is_empty() {
            return PolicyDecision::Deny("empty tool name".into());
        }

        for role in &identity.roles {
            let key = role.trim().to_ascii_lowercase();
            if let Some(compiled) = self.role_map.get(&key) {
                if compiled.all_tools
                    || compiled.allowed_tools.iter().any(|t| t == &normalized_tool)
                {
                    tracing::info!(
                        user_id = %crate::security::redact(&identity.user_id),
                        role = %key,
                        tool = %normalized_tool,
                        "IAM policy: tool access ALLOWED"
                    );
                    return PolicyDecision::Allow;
                }
            }
        }

        let reason = format!(
            "no role grants access to tool '{normalized_tool}' for user '{}'",
            crate::security::redact(&identity.user_id)
        );
        tracing::info!(
            user_id = %crate::security::redact(&identity.user_id),
            tool = %normalized_tool,
            "IAM policy: tool access DENIED"
        );
        PolicyDecision::Deny(reason)
    }

    /// Evaluate whether an identity is allowed to access a specific workspace.
    ///
    /// Deny-by-default: returns `Deny` unless at least one of the identity's
    /// roles grants access to the requested workspace.
    pub fn evaluate_workspace_access(
        &self,
        identity: &NevisIdentity,
        workspace: &str,
    ) -> PolicyDecision {
        let normalized_ws = workspace.trim().to_ascii_lowercase();
        if normalized_ws.is_empty() {
            return PolicyDecision::Deny("empty workspace name".into());
        }

        for role in &identity.roles {
            let key = role.trim().to_ascii_lowercase();
            if let Some(compiled) = self.role_map.get(&key) {
                if compiled.all_workspaces
                    || compiled
                        .allowed_workspaces
                        .iter()
                        .any(|w| w == &normalized_ws)
                {
                    tracing::info!(
                        user_id = %crate::security::redact(&identity.user_id),
                        role = %key,
                        workspace = %normalized_ws,
                        "IAM policy: workspace access ALLOWED"
                    );
                    return PolicyDecision::Allow;
                }
            }
        }

        let reason = format!(
            "no role grants access to workspace '{normalized_ws}' for user '{}'",
            crate::security::redact(&identity.user_id)
        );
        tracing::info!(
            user_id = %crate::security::redact(&identity.user_id),
            workspace = %normalized_ws,
            "IAM policy: workspace access DENIED"
        );
        PolicyDecision::Deny(reason)
    }

    /// Check if the policy has any role mappings configured.
    pub fn is_empty(&self) -> bool {
        self.role_map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mappings() -> Vec<RoleMapping> {
        vec![
            RoleMapping {
                nevis_role: "admin".into(),
                jhedaiclaw_permissions: vec!["all".into()],
                workspace_access: vec!["all".into()],
            },
            RoleMapping {
                nevis_role: "operator".into(),
                jhedaiclaw_permissions: vec![
                    "shell".into(),
                    "file_read".into(),
                    "file_write".into(),
                    "memory_search".into(),
                ],
                workspace_access: vec!["production".into(), "staging".into()],
            },
            RoleMapping {
                nevis_role: "viewer".into(),
                jhedaiclaw_permissions: vec!["file_read".into(), "memory_search".into()],
                workspace_access: vec!["staging".into()],
            },
        ]
    }

    fn identity_with_roles(roles: Vec<&str>) -> NevisIdentity {
        NevisIdentity {
            user_id: "jhedaiclaw_user".into(),
            roles: roles.into_iter().map(String::from).collect(),
            scopes: vec!["openid".into()],
            mfa_verified: true,
            session_expiry: u64::MAX,
        }
    }

    #[test]
    fn admin_gets_all_tools() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["admin"]);

        assert!(policy.evaluate_tool_access(&identity, "shell").is_allowed());
        assert!(policy
            .evaluate_tool_access(&identity, "file_read")
            .is_allowed());
        assert!(policy
            .evaluate_tool_access(&identity, "any_tool_name")
            .is_allowed());
    }

    #[test]
    fn admin_gets_all_workspaces() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["admin"]);

        assert!(policy
            .evaluate_workspace_access(&identity, "production")
            .is_allowed());
        assert!(policy
            .evaluate_workspace_access(&identity, "any_workspace")
            .is_allowed());
    }

    #[test]
    fn operator_gets_subset_of_tools() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["operator"]);

        assert!(policy.evaluate_tool_access(&identity, "shell").is_allowed());
        assert!(policy
            .evaluate_tool_access(&identity, "file_read")
            .is_allowed());
        assert!(!policy
            .evaluate_tool_access(&identity, "browser")
            .is_allowed());
    }

    #[test]
    fn operator_workspace_access_is_scoped() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["operator"]);

        assert!(policy
            .evaluate_workspace_access(&identity, "production")
            .is_allowed());
        assert!(policy
            .evaluate_workspace_access(&identity, "staging")
            .is_allowed());
        assert!(!policy
            .evaluate_workspace_access(&identity, "development")
            .is_allowed());
    }

    #[test]
    fn viewer_is_read_only() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["viewer"]);

        assert!(policy
            .evaluate_tool_access(&identity, "file_read")
            .is_allowed());
        assert!(policy
            .evaluate_tool_access(&identity, "memory_search")
            .is_allowed());
        assert!(!policy.evaluate_tool_access(&identity, "shell").is_allowed());
        assert!(!policy
            .evaluate_tool_access(&identity, "file_write")
            .is_allowed());
    }

    #[test]
    fn deny_by_default_for_unknown_role() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["unknown_role"]);

        assert!(!policy.evaluate_tool_access(&identity, "shell").is_allowed());
        assert!(!policy
            .evaluate_workspace_access(&identity, "production")
            .is_allowed());
    }

    #[test]
    fn deny_by_default_for_no_roles() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec![]);

        assert!(!policy
            .evaluate_tool_access(&identity, "file_read")
            .is_allowed());
    }

    #[test]
    fn multiple_roles_union_permissions() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["viewer", "operator"]);

        // viewer has file_read, operator has shell — both should be accessible
        assert!(policy
            .evaluate_tool_access(&identity, "file_read")
            .is_allowed());
        assert!(policy.evaluate_tool_access(&identity, "shell").is_allowed());
    }

    #[test]
    fn role_matching_is_case_insensitive() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["ADMIN"]);

        assert!(policy.evaluate_tool_access(&identity, "shell").is_allowed());
    }

    #[test]
    fn tool_matching_is_case_insensitive() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["operator"]);

        assert!(policy.evaluate_tool_access(&identity, "SHELL").is_allowed());
        assert!(policy
            .evaluate_tool_access(&identity, "File_Read")
            .is_allowed());
    }

    #[test]
    fn empty_tool_name_is_denied() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["admin"]);

        assert!(!policy.evaluate_tool_access(&identity, "").is_allowed());
        assert!(!policy.evaluate_tool_access(&identity, "  ").is_allowed());
    }

    #[test]
    fn empty_workspace_name_is_denied() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["admin"]);

        assert!(!policy.evaluate_workspace_access(&identity, "").is_allowed());
    }

    #[test]
    fn empty_mappings_deny_everything() {
        let policy = IamPolicy::from_mappings(&[]).unwrap();
        let identity = identity_with_roles(vec!["admin"]);

        assert!(policy.is_empty());
        assert!(!policy.evaluate_tool_access(&identity, "shell").is_allowed());
    }

    #[test]
    fn policy_decision_deny_contains_reason() {
        let policy = IamPolicy::from_mappings(&test_mappings()).unwrap();
        let identity = identity_with_roles(vec!["viewer"]);

        let decision = policy.evaluate_tool_access(&identity, "shell");
        match decision {
            PolicyDecision::Deny(reason) => {
                assert!(reason.contains("shell"));
            }
            PolicyDecision::Allow => panic!("expected deny"),
        }
    }

    #[test]
    fn duplicate_normalized_roles_are_rejected() {
        let mappings = vec![
            RoleMapping {
                nevis_role: "admin".into(),
                jhedaiclaw_permissions: vec!["all".into()],
                workspace_access: vec!["all".into()],
            },
            RoleMapping {
                nevis_role: " ADMIN ".into(),
                jhedaiclaw_permissions: vec!["file_read".into()],
                workspace_access: vec![],
            },
        ];
        let err = IamPolicy::from_mappings(&mappings).unwrap_err();
        assert!(
            err.to_string().contains("duplicate role mapping"),
            "Expected duplicate role error, got: {err}"
        );
    }

    #[test]
    fn empty_role_name_in_mapping_is_skipped() {
        let mappings = vec![RoleMapping {
            nevis_role: "  ".into(),
            jhedaiclaw_permissions: vec!["all".into()],
            workspace_access: vec![],
        }];
        let policy = IamPolicy::from_mappings(&mappings).unwrap();
        assert!(policy.is_empty());
    }
}
