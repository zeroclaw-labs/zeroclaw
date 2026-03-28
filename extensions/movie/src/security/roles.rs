use crate::config::SecurityRoleConfig;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAccess {
    pub allowed: bool,
    pub requires_totp: bool,
}

#[derive(Debug, Clone)]
struct RoleDefinition {
    allowed_tools: Vec<String>,
    denied_tools: Vec<String>,
    totp_gated: Vec<String>,
    inherits: Option<String>,
    use_global_gated_actions: bool,
}

#[derive(Debug, Clone)]
pub struct RoleRegistry {
    roles: HashMap<String, RoleDefinition>,
}

impl RoleRegistry {
    #[must_use]
    pub fn built_in() -> Self {
        let mut roles = HashMap::new();

        roles.insert(
            "owner".to_string(),
            RoleDefinition {
                allowed_tools: vec!["*".to_string()],
                denied_tools: Vec::new(),
                totp_gated: Vec::new(),
                inherits: None,
                use_global_gated_actions: true,
            },
        );

        roles.insert(
            "admin".to_string(),
            RoleDefinition {
                allowed_tools: vec!["*".to_string()],
                denied_tools: Vec::new(),
                totp_gated: Vec::new(),
                inherits: None,
                use_global_gated_actions: true,
            },
        );

        roles.insert(
            "operator".to_string(),
            RoleDefinition {
                allowed_tools: vec!["*".to_string()],
                denied_tools: vec![
                    "memory_forget".to_string(),
                    "users_manage".to_string(),
                    "roles_manage".to_string(),
                ],
                totp_gated: vec![
                    "shell".to_string(),
                    "file_write".to_string(),
                    "browser_open".to_string(),
                    "browser".to_string(),
                ],
                inherits: None,
                use_global_gated_actions: false,
            },
        );

        roles.insert(
            "viewer".to_string(),
            RoleDefinition {
                allowed_tools: vec!["file_read".to_string(), "memory_search".to_string()],
                denied_tools: Vec::new(),
                totp_gated: Vec::new(),
                inherits: None,
                use_global_gated_actions: false,
            },
        );

        roles.insert(
            "guest".to_string(),
            RoleDefinition {
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                totp_gated: Vec::new(),
                inherits: None,
                use_global_gated_actions: false,
            },
        );

        Self { roles }
    }

    pub fn from_config(custom_roles: &[SecurityRoleConfig]) -> Result<Self> {
        let mut registry = Self::built_in();
        for role in custom_roles {
            let normalized_name = role.name.trim().to_ascii_lowercase();
            if normalized_name.is_empty() {
                continue;
            }

            let inherits = role
                .inherits
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase);

            registry.roles.insert(
                normalized_name,
                RoleDefinition {
                    allowed_tools: role.allowed_tools.clone(),
                    denied_tools: role.denied_tools.clone(),
                    totp_gated: role.totp_gated.clone(),
                    inherits,
                    use_global_gated_actions: false,
                },
            );
        }

        registry.validate_inheritance()?;
        Ok(registry)
    }

    #[must_use]
    pub fn resolve_tool_access(
        &self,
        role_name: &str,
        tool_name: &str,
        global_gated_actions: &[String],
    ) -> ToolAccess {
        let normalized_role = role_name.trim().to_ascii_lowercase();
        let normalized_tool = tool_name.trim();

        if normalized_role.is_empty() || normalized_tool.is_empty() {
            return ToolAccess {
                allowed: false,
                requires_totp: false,
            };
        }

        let Some(role) = self.roles.get(&normalized_role) else {
            return ToolAccess {
                allowed: false,
                requires_totp: false,
            };
        };

        let mut seen = Vec::new();
        let allowed = self
            .resolve_allow_decision(role, normalized_tool, &mut seen)
            .unwrap_or(false);
        if !allowed {
            return ToolAccess {
                allowed: false,
                requires_totp: false,
            };
        }

        let mut seen_totp = Vec::new();
        let role_totp = self.tool_in_totp_list(role, normalized_tool, &mut seen_totp);
        let mut seen_global = Vec::new();
        let uses_global = self.uses_global_gated_actions(role, &mut seen_global);
        let global_totp = uses_global && matches_tool(global_gated_actions, normalized_tool);

        ToolAccess {
            allowed: true,
            requires_totp: role_totp || global_totp,
        }
    }

    fn resolve_allow_decision(
        &self,
        role: &RoleDefinition,
        tool_name: &str,
        seen_roles: &mut Vec<String>,
    ) -> Option<bool> {
        if matches_tool(&role.denied_tools, tool_name) {
            return Some(false);
        }
        if matches_tool(&role.allowed_tools, tool_name) {
            return Some(true);
        }
        let parent_name = role.inherits.as_deref()?;
        if seen_roles.iter().any(|entry| entry == parent_name) {
            return None;
        }
        seen_roles.push(parent_name.to_string());
        let decision = self
            .roles
            .get(parent_name)
            .and_then(|parent| self.resolve_allow_decision(parent, tool_name, seen_roles));
        seen_roles.pop();
        decision
    }

    fn tool_in_totp_list(
        &self,
        role: &RoleDefinition,
        tool_name: &str,
        seen_roles: &mut Vec<String>,
    ) -> bool {
        if matches_tool(&role.totp_gated, tool_name) {
            return true;
        }
        let Some(parent_name) = role.inherits.as_deref() else {
            return false;
        };
        if seen_roles.iter().any(|entry| entry == parent_name) {
            return false;
        }
        seen_roles.push(parent_name.to_string());
        let inherited = self
            .roles
            .get(parent_name)
            .is_some_and(|parent| self.tool_in_totp_list(parent, tool_name, seen_roles));
        seen_roles.pop();
        inherited
    }

    fn uses_global_gated_actions(
        &self,
        role: &RoleDefinition,
        seen_roles: &mut Vec<String>,
    ) -> bool {
        if role.use_global_gated_actions {
            return true;
        }
        let Some(parent_name) = role.inherits.as_deref() else {
            return false;
        };
        if seen_roles.iter().any(|entry| entry == parent_name) {
            return false;
        }
        seen_roles.push(parent_name.to_string());
        let inherited = self
            .roles
            .get(parent_name)
            .is_some_and(|parent| self.uses_global_gated_actions(parent, seen_roles));
        seen_roles.pop();
        inherited
    }

    fn validate_inheritance(&self) -> Result<()> {
        for (name, role) in &self.roles {
            if let Some(parent) = role.inherits.as_deref() {
                if !self.roles.contains_key(parent) {
                    bail!("role '{name}' inherits unknown parent '{parent}'");
                }
            }
        }

        let mut marks: HashMap<&str, u8> = HashMap::new();
        for name in self.roles.keys() {
            Self::visit(name, &self.roles, &mut marks)?;
        }
        Ok(())
    }

    fn visit<'a>(
        name: &'a str,
        roles: &'a HashMap<String, RoleDefinition>,
        marks: &mut HashMap<&'a str, u8>,
    ) -> Result<()> {
        if marks.get(name).copied() == Some(2) {
            return Ok(());
        }
        if marks.get(name).copied() == Some(1) {
            return Err(anyhow!("role inheritance cycle detected at '{name}'"));
        }
        marks.insert(name, 1);
        if let Some(parent) = roles.get(name).and_then(|role| role.inherits.as_deref()) {
            Self::visit(parent, roles, marks)?;
        }
        marks.insert(name, 2);
        Ok(())
    }
}

fn matches_tool(rules: &[String], tool_name: &str) -> bool {
    rules
        .iter()
        .map(|rule| rule.trim())
        .filter(|rule| !rule.is_empty())
        .any(|rule| rule == "*" || rule.eq_ignore_ascii_case(tool_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_operator_permissions_gate_shell() {
        let registry = RoleRegistry::built_in();
        let shell = registry.resolve_tool_access("operator", "shell", &[]);
        assert!(shell.allowed);
        assert!(shell.requires_totp);

        let memory_forget = registry.resolve_tool_access("operator", "memory_forget", &[]);
        assert!(!memory_forget.allowed);
    }

    #[test]
    fn built_in_viewer_is_read_only() {
        let registry = RoleRegistry::built_in();
        let file_read = registry.resolve_tool_access("viewer", "file_read", &[]);
        assert!(file_read.allowed);
        assert!(!file_read.requires_totp);

        let shell = registry.resolve_tool_access("viewer", "shell", &[]);
        assert!(!shell.allowed);
    }

    #[test]
    fn owner_uses_global_gated_actions_for_totp() {
        let registry = RoleRegistry::built_in();
        let global = vec!["shell".to_string(), "browser_open".to_string()];

        let shell = registry.resolve_tool_access("owner", "shell", &global);
        assert!(shell.allowed);
        assert!(shell.requires_totp);

        let file_read = registry.resolve_tool_access("owner", "file_read", &global);
        assert!(file_read.allowed);
        assert!(!file_read.requires_totp);
    }

    #[test]
    fn custom_role_inherits_parent_allowlist_and_totp() {
        let registry = RoleRegistry::from_config(&[SecurityRoleConfig {
            name: "developer".to_string(),
            allowed_tools: vec!["git".to_string()],
            denied_tools: vec!["memory_forget".to_string()],
            totp_gated: vec!["git".to_string()],
            inherits: Some("operator".to_string()),
            ..SecurityRoleConfig::default()
        }])
        .expect("registry from config");

        let git = registry.resolve_tool_access("developer", "git", &[]);
        assert!(git.allowed);
        assert!(git.requires_totp);

        let shell = registry.resolve_tool_access("developer", "shell", &[]);
        assert!(shell.allowed);
        assert!(shell.requires_totp);

        let memory_forget = registry.resolve_tool_access("developer", "memory_forget", &[]);
        assert!(!memory_forget.allowed);
    }

    #[test]
    fn inheritance_cycle_is_rejected() {
        let result = RoleRegistry::from_config(&[
            SecurityRoleConfig {
                name: "role_a".to_string(),
                inherits: Some("role_b".to_string()),
                ..SecurityRoleConfig::default()
            },
            SecurityRoleConfig {
                name: "role_b".to_string(),
                inherits: Some("role_a".to_string()),
                ..SecurityRoleConfig::default()
            },
        ]);
        assert!(result.is_err());
        assert!(result.expect_err("error").to_string().contains("cycle"));
    }
}
